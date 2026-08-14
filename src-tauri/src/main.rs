// Prevents an extra console window on Windows in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod agent;
mod engine;
mod remote;

use std::path::PathBuf;
use std::sync::Arc;

use agent::context::{ContextManager, UsageReport, DEFAULT_LIMIT};
use agent::interrupt::{AbortPayload, InterruptState};
use agent::skills::{KnowledgeReport, KnowledgeState};
use agent::{ToolCall, ToolResult, ToolState};
use crossbeam_channel::bounded;
use engine::{
    load_engine, InferenceDone, InferenceRequest, LoadProgressEvent, LocalGenerator, ModelInfo,
    ModelInitParams, TextGenerator, WorkerEvent,
};
use remote::{RemoteGenerator, RemoteModelConfig};
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_dialog::DialogExt;
use tokio::sync::Mutex;

/// Shared inference state. The engine lives in the mutex so that:
///  * model load (blocking) can run in `spawn_blocking`,
///  * a generation can run on its own native thread holding the lock for its
///    entire duration,
///  * the UI thread never blocks on llama.cpp.
struct InferenceState {
    engine: Mutex<Option<Box<dyn TextGenerator>>>,
    info: Mutex<Option<ModelInfo>>,
    worker_tx: Mutex<Option<crossbeam_channel::Sender<WorkerEvent>>>,
}

impl Default for InferenceState {
    fn default() -> Self {
        Self {
            engine: Mutex::new(None),
            info: Mutex::new(None),
            worker_tx: Mutex::new(None),
        }
    }
}

/// The token-budget engine managed as Tauri state.
pub struct ContextState {
    inner: Mutex<ContextManager>,
}

impl Default for ContextState {
    fn default() -> Self {
        Self {
            inner: Mutex::new(ContextManager::new(DEFAULT_LIMIT)),
        }
    }
}

/// Frontend -> worker bridge. This channel is *bounded*: llama.cpp decode
/// pauses when the webview cannot keep up, which is exactly the backpressure
/// we want. The receiving loop forwards every event to the emitter task.
fn spawn_emitter(app: AppHandle) -> crossbeam_channel::Sender<WorkerEvent> {
    let (tx, rx) = bounded::<WorkerEvent>(256);
    let app_sink = app.clone();
    std::thread::spawn(move || {
        while let Ok(event) = rx.recv() {
            match event {
                WorkerEvent::Token { session_id, delta } => {
                    let _ = app_sink.emit("inference-token", TokenEvent { session_id, delta });
                }
                WorkerEvent::Step { session_id, step } => {
                    let _ = app_sink.emit("agent-step", StepEvent { session_id, step });
                }
                WorkerEvent::Subtask { session_id, subtask } => {
                    let _ = app_sink.emit("agent-subtask", SubtaskEvent { session_id, subtask });
                }
                WorkerEvent::Done { session_id, done } => {
                    let _ = app_sink.emit("inference-done", DoneEvent { session_id, done });
                }
                WorkerEvent::Error { session_id, message } => {
                    let _ = app_sink.emit("inference-error", ErrorEvent { session_id, message });
                }
            }
        }
    });
    tx
}

#[derive(serde::Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct TokenEvent {
    session_id: u64,
    delta: String,
}

#[derive(serde::Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct StepEvent {
    session_id: u64,
    step: engine::StepStat,
}

#[derive(serde::Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct SubtaskEvent {
    session_id: u64,
    subtask: engine::SubtaskStat,
}

#[derive(serde::Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct DoneEvent {
    session_id: u64,
    done: InferenceDone,
}

#[derive(serde::Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct ErrorEvent {
    session_id: u64,
    message: String,
}

// ---------------------------------------------------------------------------
// Model lifecycle
// ---------------------------------------------------------------------------

/// Pick a GGUF model file, load it and return its metadata. Blocking model
/// load is moved to `spawn_blocking` so the webview stays responsive.
#[tauri::command]
async fn pick_and_load_model(
    app: AppHandle,
    state: State<'_, InferenceState>,
    context_state: State<'_, ContextState>,
    params: Option<ModelInitParams>,
) -> Result<Option<ModelInfo>, String> {
    let dialog = app.dialog();
    let Some(picked) = dialog
        .file()
        .set_title("Select a GGUF model")
        .add_filter("GGUF model", &["gguf"])
        .blocking_pick_file()
    else {
        return Ok(None);
    };
    let path = picked.into_path().map_err(|e| e.to_string())?;

    let engine_guard = state.engine.lock().await;
    let info_guard = state.info.lock().await;
    drop(info_guard);
    drop(engine_guard);

    let params = params.unwrap_or_default();
    let params_clone = params.clone();

    // Make sure the frontend learns of the full path the user picked.
    let path_for_event = path.clone();
    let app_for_load = app.clone();

    let engine = tokio::task::spawn_blocking(move || load_engine(&path, &params_clone, &app_for_load))
        .await
        .map_err(|e| format!("Model load task panicked: {e}"))?
        .map_err(|e| {
            let _ = app.emit_to(
                "main",
                "model-load-progress",
                LoadProgressEvent { stage: "error", progress: 0.0 },
            );
            e
        })?;

    let info = engine.info();
    let mut engine_guard = state.engine.lock().await;
    let mut info_guard = state.info.lock().await;
    *engine_guard = Some(Box::new(LocalGenerator::new(engine)));
    *info_guard = Some(info.clone());

    // Align the eviction engine's budget with the loaded model's context.
    context_state.inner.lock().await.set_limit(info.context_size as usize);

    let _ = app.emit_to("main", "model-load-progress", LoadProgressEvent { stage: "done", progress: 1.0 });
    let _ = app.emit("model-loaded", &info);
    let _ = app.emit("model-path", &path_for_event);
    drop(info_guard);
    drop(engine_guard);
    Ok(Some(info))
}

/// Fetch the available model ids for a remote provider so the connection UI
/// can offer them as a dropdown (see `remote::list_models`).
#[tauri::command]
async fn list_remote_models(config: RemoteModelConfig) -> Result<Vec<String>, String> {
    remote::list_models(&config).await
}

/// Configure and activate the remote (OpenAI-compatible) backend. Swaps out
/// any local engine, aligns the context budget, and fires the same
/// `model-loaded` flow as a local load so the UI stays backend-agnostic.
#[tauri::command]
async fn configure_remote_model(
    app: AppHandle,
    state: State<'_, InferenceState>,
    interrupt_state: State<'_, InterruptState>,
    context_state: State<'_, ContextState>,
    config: RemoteModelConfig,
) -> Result<ModelInfo, String> {
    interrupt_state.trigger();
    let gen = RemoteGenerator::new(config)?;
    let info = gen.info();

    let mut engine_guard = state.engine.lock().await;
    let mut info_guard = state.info.lock().await;
    *engine_guard = Some(Box::new(gen));
    *info_guard = Some(info.clone());
    context_state
        .inner
        .lock()
        .await
        .set_limit(info.context_size as usize);
    drop(info_guard);
    drop(engine_guard);

    let _ = app.emit_to("main", "model-load-progress", LoadProgressEvent { stage: "done", progress: 1.0 });
    let _ = app.emit("model-loaded", &info);
    Ok(info)
}

#[tauri::command]
async fn unload_model(
    state: State<'_, InferenceState>,
    interrupt_state: State<'_, InterruptState>,
) -> Result<(), String> {
    interrupt_state.trigger();
    let engine_guard = state.engine.lock().await;
    let info_guard = state.info.lock().await;
    drop(info_guard);
    drop(engine_guard);
    let mut engine_guard = state.engine.lock().await;
    let mut info_guard = state.info.lock().await;
    *engine_guard = None;
    *info_guard = None;
    drop(info_guard);
    drop(engine_guard);
    Ok(())
}

/// Poll the current model metadata (used on webview startup / reconnect).
#[tauri::command]
async fn model_status(state: State<'_, InferenceState>) -> Result<Option<ModelInfo>, String> {
    let info_guard = state.info.lock().await;
    Ok(info_guard.clone())
}

/// Streaming inference. Spawns a dedicated native thread that holds the engine
/// lock for the whole generation (see `StandaloneEngine` soundness note).
/// A fresh circuit-breaker token is armed per run so an old cancellation can
/// never leak into this one.
#[tauri::command]
async fn stream_inference(
    app: AppHandle,
    state: State<'_, InferenceState>,
    interrupt_state: State<'_, InterruptState>,
    request: InferenceRequest,
) -> Result<u64, String> {
    let mut engine_guard = state.engine.lock().await;
    let Some(engine) = engine_guard.as_mut() else {
        return Err("No model loaded".to_string());
    };

    let session_id = interrupt_state.next_session();
    let interrupt = interrupt_state.arm();
    let _ = app.emit("inference-started", StartedEvent { session_id });

    let tx = state.worker_tx.lock().await.take().unwrap_or_else(|| spawn_emitter(app.clone()));

    // SAFETY: `engine` is pinned inside `InferenceState.engine` for the app
    // lifetime and only ever touched by one worker thread at a time (the tokio
    // mutex serializes access; the worker exits before the next job starts).
    let backend_ref: &'static mut Box<dyn TextGenerator> = unsafe {
        std::mem::transmute::<&mut Box<dyn TextGenerator>, &'static mut Box<dyn TextGenerator>>(engine)
    };
    let tx_clone = tx.clone();

    std::thread::spawn(move || {
        let gen: &mut dyn TextGenerator = &mut **backend_ref;
        let result = gen.generate(&request, session_id, &interrupt, &tx_clone);
        let _ = tx_clone.send(match result {
            Ok(outcome) => WorkerEvent::Done { session_id, done: outcome.done },
            Err(message) => WorkerEvent::Error { session_id, message },
        });
    });

    drop(engine_guard);
    *state.worker_tx.lock().await = Some(tx);

    Ok(session_id)
}

/// Agentic task execution: an orchestrated generate → tool-call → feedback
/// loop (see `agent::orchestrator`). The whole loop runs on one native thread
/// that holds the engine lock per step, so intermediate tool dispatches never
/// touch llama.cpp. The circuit breaker token is armed once and shared with
/// both generation and tool sub-processes.
#[tauri::command]
async fn agent_run_task(
    app: AppHandle,
    state: State<'_, InferenceState>,
    interrupt_state: State<'_, InterruptState>,
    context_state: State<'_, ContextState>,
    tool_state: State<'_, ToolState>,
    request: agent::orchestrator::AgentTaskRequest,
) -> Result<u64, String> {
    let mut engine_guard = state.engine.lock().await;
    let Some(engine) = engine_guard.as_mut() else {
        return Err("No model loaded".to_string());
    };

    let session_id = interrupt_state.next_session();
    let interrupt = interrupt_state.arm();
    let _ = app.emit("inference-started", StartedEvent { session_id });

    let tx = state
        .worker_tx
        .lock()
        .await
        .take()
        .unwrap_or_else(|| spawn_emitter(app.clone()));

    // SAFETY: `engine` is pinned inside `InferenceState.engine` for the app
    // lifetime and only ever touched by one worker thread at a time (the tokio
    // mutex + per-run tokenization of the orchestrator serializes access).
    let backend_ref: &'static mut Box<dyn TextGenerator> = unsafe {
        std::mem::transmute::<&mut Box<dyn TextGenerator>, &'static mut Box<dyn TextGenerator>>(engine)
    };
    // SAFETY: `ToolState` is Tauri-managed and outlives the app; the worker
    // thread only reads its workspace root + MCP cache.
    let tool_state_ref: &'static ToolState = unsafe {
        std::mem::transmute::<&ToolState, &'static ToolState>(tool_state.inner())
    };

    let context_snapshot = context_state.inner.lock().await.messages();
    let app_for_thread = app.clone();
    let tx_clone = tx.clone();
    let context_budget = engine.info().context_size as usize;

    std::thread::spawn(move || {
        let gen: &mut dyn TextGenerator = &mut **backend_ref;
        let result = agent::orchestrator::run_agent_loop(
            gen,
            tool_state_ref,
            &app_for_thread,
            &interrupt,
            &tx_clone,
            session_id,
            &context_snapshot,
            &request,
            context_budget,
        );
        let _ = tx_clone.send(match result {
            Ok(outcome) => WorkerEvent::Done { session_id, done: outcome.done },
            Err(message) => WorkerEvent::Error { session_id, message },
        });
    });

    drop(engine_guard);
    *state.worker_tx.lock().await = Some(tx);

    Ok(session_id)
}

#[derive(serde::Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct StartedEvent {
    session_id: u64,
}

/// Cancel the in-flight generation (if any) via the circuit breaker.
#[tauri::command]
async fn cancel_inference(state: State<'_, InterruptState>) -> Result<(), String> {
    state.trigger();
    Ok(())
}

/// Emergency abort for any running job (LLM generation, terminal sub-process,
/// MCP call). Cancels the circuit breaker instantly and returns an
/// "Execution Aborted" payload to the UI.
#[tauri::command]
async fn abort_agent_execution(
    app: AppHandle,
    state: State<'_, InterruptState>,
) -> Result<AbortPayload, String> {
    state.trigger();
    let payload = state.payload(state.current_session());
    let _ = app.emit("execution-aborted", &payload);
    Ok(payload)
}

// ---------------------------------------------------------------------------
// Workspace / file I/O (host-side; the webview has no direct fs access)
// ---------------------------------------------------------------------------

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct FsEntry {
    name: String,
    path: String,
    is_dir: bool,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct TextFile {
    path: String,
    content: String,
}

#[tauri::command]
async fn pick_workspace_folder(app: AppHandle) -> Result<Option<String>, String> {
    let picked = app.dialog().file().blocking_pick_folder();
    Ok(picked
        .and_then(|p| p.into_path().ok())
        .map(|p| p.to_string_lossy().into_owned()))
}

#[tauri::command]
async fn list_directory(_app: AppHandle, root: String, relative: Option<String>) -> Result<Vec<FsEntry>, String> {
    let base = PathBuf::from(&root);
    let dir = match relative {
        Some(ref r) if !r.is_empty() => base.join(r),
        _ => base.clone(),
    };
    if !dir.is_dir() {
        return Err(format!("Not a directory: {}", dir.display()));
    }
    let mut entries = Vec::new();
    for entry in std::fs::read_dir(&dir).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let is_dir = entry.file_type().map_err(|e| e.to_string())?.is_dir();
        let rel = if relative.is_some() {
            dir.join(&name).to_string_lossy().into_owned()
        } else {
            entry.path().to_string_lossy().into_owned()
        };
        entries.push(FsEntry { name, path: rel, is_dir });
    }
    entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then(a.name.cmp(&b.name)));
    Ok(entries)
}

#[tauri::command]
async fn read_text_file(_app: AppHandle, path: String) -> Result<TextFile, String> {
    let bytes = tokio::fs::read(&path).await.map_err(|e| e.to_string())?;
    // Accept UTF-8 and common single-byte encodings; never fail on odd files.
    let content = match String::from_utf8(bytes.clone()) {
        Ok(s) => s,
        Err(_) => encoding_rs::WINDOWS_1252.decode(&bytes).0.to_string(),
    };
    Ok(TextFile { path, content })
}

#[tauri::command]
async fn write_text_file(path: String, content: String) -> Result<(), String> {
    tokio::fs::write(&path, content).await.map_err(|e| e.to_string())?;
    Ok(())
}

/// Save-as dialog for untitled buffers; returns the chosen path or `None` if
/// the user cancelled.
#[tauri::command]
async fn save_file_as(app: AppHandle, content: String) -> Result<Option<String>, String> {
    let picked = app
        .dialog()
        .file()
        .set_title("Save file as")
        .add_filter("All files", &["*"])
        .blocking_save_file();
    let Some(picked) = picked else {
        return Ok(None);
    };
    let path = picked.into_path().map_err(|e| e.to_string())?;
    tokio::fs::write(&path, content).await.map_err(|e| e.to_string())?;
    Ok(Some(path.to_string_lossy().into_owned()))
}

// ---------------------------------------------------------------------------
// Agent core
// ---------------------------------------------------------------------------

/// Set the active workspace root that file tools operate on by default.
/// Also scans project skills/rules and syncs them into the context manager.
#[tauri::command]
async fn agent_set_workspace(
    app: AppHandle,
    state: State<'_, ToolState>,
    context_state: State<'_, ContextState>,
    knowledge_state: State<'_, Arc<KnowledgeState>>,
    root: String,
) -> Result<(), String> {
    let p = PathBuf::from(&root);
    if !p.is_dir() {
        return Err(format!("Not a directory: {}", p.display()));
    }
    *state.workspace.lock().await = Some(p.clone());
    let config_dir = app_config_dir(&app);
    let _ = knowledge_state.scan(&p, &config_dir);
    sync_knowledge(&context_state, &knowledge_state).await;
    let _ = app.emit("agent-knowledge", knowledge_report(&knowledge_state));
    Ok(())
}

/// Get the currently configured workspace root (if any).
#[tauri::command]
async fn agent_get_workspace(state: State<'_, ToolState>) -> Result<Option<String>, String> {
    let guard = state.workspace.lock().await;
    Ok(guard.as_ref().map(|p| p.to_string_lossy().into_owned()))
}

/// Execute a single tool call. Real-time progress is pushed over the
/// `agent://tool-event` event channel; the returned `ToolResult` is the
/// authoritative outcome. The circuit breaker token is handed to the tool so
/// sub-processes can be aborted mid-flight.
#[tauri::command]
async fn agent_execute_tool(
    app: AppHandle,
    state: State<'_, ToolState>,
    interrupt_state: State<'_, InterruptState>,
    call: ToolCall,
) -> Result<ToolResult, String> {
    agent::tools::dispatch(&app, &state, &call, interrupt_state.current()).await
}

/// Execute a batch of tool calls sequentially, returning results in order.
/// The orchestrator can fire multiple independent reads/writes in one trip.
#[tauri::command]
async fn agent_batch_execute(
    app: AppHandle,
    state: State<'_, ToolState>,
    interrupt_state: State<'_, InterruptState>,
    calls: Vec<ToolCall>,
) -> Result<Vec<ToolResult>, String> {
    let mut results = Vec::with_capacity(calls.len());
    for call in calls {
        results.push(agent::tools::dispatch(&app, &state, &call, interrupt_state.current()).await?);
    }
    Ok(results)
}

/// JSON schemas describing every tool, for orchestrator-side validation.
#[tauri::command]
async fn agent_tool_schemas() -> Result<std::collections::HashMap<String, serde_json::Value>, String> {
    let mut schemas = std::collections::HashMap::new();
    for (name, schema) in agent::core::tool_schemas() {
        schemas.insert(name.to_string(), schema);
    }
    Ok(schemas)
}

/// Drop all cached MCP server connections (they respawn on demand).
#[tauri::command]
async fn agent_reset_mcp(state: State<'_, ToolState>) -> Result<(), String> {
    state.mcp_servers.lock().await.clear();
    Ok(())
}

// ---------------------------------------------------------------------------
// Skills & Rules
// ---------------------------------------------------------------------------

fn app_config_dir(app: &AppHandle) -> PathBuf {
    app.path()
        .app_config_dir()
        .unwrap_or_else(|_| std::env::temp_dir().join("ai-editor"))
}

fn app_data_dir(app: &AppHandle) -> PathBuf {
    app.path()
        .app_data_dir()
        .unwrap_or_else(|_| std::env::temp_dir().join("ai-editor-data"))
}

/// Push rules + active skills into the context manager as pinned buffers.
async fn sync_knowledge(context_state: &ContextState, knowledge: &KnowledgeState) {
    let report = knowledge.report();
    let mut inner = context_state.inner.lock().await;
    if report.rules.trim().is_empty() {
        inner.remove_pinned("rules");
    } else {
        inner.upsert_pinned("rules", report.rules.clone());
    }
    let skills = knowledge.active_skills_content();
    if skills.trim().is_empty() {
        inner.remove_pinned("skill");
    } else {
        inner.upsert_pinned("skill", skills);
    }
}

fn knowledge_report(knowledge: &KnowledgeState) -> KnowledgeReport {
    knowledge.report()
}

/// (Re)scan the workspace for `.ai/skills`, `.ai/rules`, AGENTS.md, etc.
#[tauri::command]
async fn knowledge_scan(
    app: AppHandle,
    state: State<'_, ToolState>,
    context_state: State<'_, ContextState>,
    knowledge_state: State<'_, Arc<KnowledgeState>>,
) -> Result<KnowledgeReport, String> {
    let ws = {
        let guard = state.workspace.lock().await;
        guard.clone().ok_or("No workspace set - open a workspace first")?
    };
    let config_dir = app_config_dir(&app);
    knowledge_state.scan(&ws, &config_dir)?;
    sync_knowledge(&context_state, &knowledge_state).await;
    Ok(knowledge_report(&knowledge_state))
}

/// Current knowledge snapshot (rules + all skills with active flags).
#[tauri::command]
async fn knowledge_report_cmd(
    knowledge_state: State<'_, Arc<KnowledgeState>>,
) -> Result<KnowledgeReport, String> {
    Ok(knowledge_report(&knowledge_state))
}

/// Toggle a skill in/out of the active context.
#[tauri::command]
async fn skill_set_active(
    app: AppHandle,
    knowledge_state: State<'_, Arc<KnowledgeState>>,
    context_state: State<'_, ContextState>,
    name: String,
    active: bool,
) -> Result<KnowledgeReport, String> {
    knowledge_state.set_active(&name, active)?;
    sync_knowledge(&context_state, &knowledge_state).await;
    let _ = app.emit("agent-knowledge", knowledge_report(&knowledge_state));
    Ok(knowledge_report(&knowledge_state))
}

// ---------------------------------------------------------------------------
// Permissions
// ---------------------------------------------------------------------------

/// Complete a pending permission-approval request from the UI.
#[tauri::command]
async fn agent_respond_permission(
    state: State<'_, ToolState>,
    request_id: String,
    allowed: bool,
) -> Result<(), String> {
    let mut reqs = state.permission_requests.lock().await;
    if let Some(tx) = reqs.remove(&request_id) {
        let _ = tx.send(allowed);
    }
    Ok(())
}

/// Effective policy snapshot for the UI.
#[tauri::command]
async fn agent_policy_snapshot(state: State<'_, ToolState>) -> Result<Value, String> {
    let ws = state.workspace.lock().await.clone();
    Ok(agent::policy::snapshot(ws.as_deref()))
}

// ---------------------------------------------------------------------------
// Persistence: settings + session history
// ---------------------------------------------------------------------------

#[tauri::command]
async fn settings_load(app: AppHandle) -> Result<Value, String> {
    let dir = app_data_dir(&app);
    let path = dir.join("settings.json");
    match std::fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str(&text).map_err(|e| e.to_string()),
        Err(_) => Ok(json!({})),
    }
}

#[tauri::command]
async fn settings_save(app: AppHandle, settings: Value) -> Result<(), String> {
    let dir = app_data_dir(&app);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let text = serde_json::to_string_pretty(&settings).map_err(|e| e.to_string())?;
    std::fs::write(dir.join("settings.json"), text).map_err(|e| e.to_string())
}

/// Append one conversation record to the project's JSONL session log.
#[tauri::command]
async fn session_append(app: AppHandle, project: String, record: Value) -> Result<(), String> {
    let dir = app_data_dir(&app).join("sessions");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let safe = project
        .replace(['/', '\\', ':'], "_")
        .trim_matches('_')
        .to_string();
    let file = dir.join(format!("{safe}.jsonl"));
    let mut line = serde_json::to_string(&record).map_err(|e| e.to_string())?;
    line.push('\n');
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(file)
        .map_err(|e| e.to_string())?;
    f.write_all(line.as_bytes()).map_err(|e| e.to_string())
}

/// Load the full history for a project (all JSONL records in order).
#[tauri::command]
async fn session_load(app: AppHandle, project: String) -> Result<Vec<Value>, String> {
    let dir = app_data_dir(&app).join("sessions");
    let safe = project
        .replace(['/', '\\', ':'], "_")
        .trim_matches('_')
        .to_string();
    let file = dir.join(format!("{safe}.jsonl"));
    let Ok(text) = std::fs::read_to_string(&file) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for line in text.lines() {
        if let Ok(v) = serde_json::from_str::<Value>(line) {
            out.push(v);
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Context eviction engine
// ---------------------------------------------------------------------------

/// Current token-budget report (total, limit, 80% threshold, evictions).
#[tauri::command]
async fn context_status(state: State<'_, ContextState>) -> Result<UsageReport, String> {
    Ok(state.inner.lock().await.usage())
}

/// Pin (or replace) the system prompt; it is never evicted.
#[tauri::command]
async fn context_set_system_prompt(
    state: State<'_, ContextState>,
    content: String,
) -> Result<UsageReport, String> {
    let mut inner = state.inner.lock().await;
    inner.set_system_prompt(content);
    Ok(inner.usage())
}

/// Pin (or replace) the active-file context buffer; it is never evicted.
#[tauri::command]
async fn context_set_file_buffer(
    state: State<'_, ContextState>,
    content: String,
) -> Result<UsageReport, String> {
    let mut inner = state.inner.lock().await;
    inner.set_file_buffer(content);
    Ok(inner.usage())
}

/// Append an evictable conversation turn; auto-triggers sliding-window
/// truncation when usage crosses 80% of the model's context limit.
#[tauri::command]
async fn context_push_turn(
    state: State<'_, ContextState>,
    role: String,
    content: String,
) -> Result<UsageReport, String> {
    let mut inner = state.inner.lock().await;
    Ok(inner.push(&role, content))
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // One shared knowledge snapshot backs both the UI commands and the agent's
    // `read_skill` tool, so a scan is immediately visible to both sides.
    let knowledge: std::sync::Arc<KnowledgeState> = std::sync::Arc::new(KnowledgeState::default());
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(InferenceState::default())
        .manage(ToolState {
            knowledge: knowledge.clone(),
            ..ToolState::default()
        })
        .manage(InterruptState::default())
        .manage(ContextState::default())
        .manage(knowledge)
        .invoke_handler(tauri::generate_handler![
            pick_and_load_model,
            configure_remote_model,
            list_remote_models,
            unload_model,
            model_status,
            stream_inference,
            cancel_inference,
            abort_agent_execution,
            agent_run_task,
            pick_workspace_folder,
            list_directory,
            read_text_file,
            write_text_file,
            save_file_as,
            agent_set_workspace,
            agent_get_workspace,
            agent_execute_tool,
            agent_batch_execute,
            agent_tool_schemas,
            agent_reset_mcp,
            knowledge_scan,
            knowledge_report_cmd,
            skill_set_active,
            agent_respond_permission,
            agent_policy_snapshot,
            settings_load,
            settings_save,
            session_append,
            session_load,
            context_status,
            context_set_system_prompt,
            context_set_file_buffer,
            context_push_turn
        ])
        .run(tauri::generate_context!())
        .expect("error while running AI Editor");
}

fn main() {
    run();
}

