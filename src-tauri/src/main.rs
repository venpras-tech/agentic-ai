// Prevents an extra console window on Windows in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod agent;
mod engine;
mod remote;

use std::path::PathBuf;

use agent::context::{ContextManager, UsageReport, DEFAULT_LIMIT};
use agent::interrupt::{AbortPayload, InterruptState};
use agent::skills::{KnowledgeReport, KnowledgeState};
use agent::{PermissionDecision, ToolCall, ToolResult, ToolState};
use crossbeam_channel::{bounded, Sender};
use engine::{
    EnginePool, InferenceDone, InferenceRequest, LoadProgressEvent, LocalGenerator, ModelInfo,
    ModelInitParams, TextGenerator, WorkerEvent,
};
use remote::{RemoteGenerator, RemoteModelConfig};
use serde_json::{json, Value};
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_dialog::DialogExt;
use tokio::sync::Mutex;

/// Shared inference state. The engine lives as an [`EnginePool`] — one worker
/// thread per generator, each owning its context/client for its whole life.
/// Model load (blocking) runs in `spawn_blocking`; generations dispatch to
/// workers over channels, so there is no `'static` transmute of the engine and
/// several generations can run concurrently (parallel sub-tasks).
struct InferenceState {
    pool: Mutex<Option<Arc<EnginePool>>>,
    info: Mutex<Option<ModelInfo>>,
    worker_tx: Mutex<Option<crossbeam_channel::Sender<WorkerEvent>>>,
}

impl Default for InferenceState {
    fn default() -> Self {
        Self {
            pool: Mutex::new(None),
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

/// Local pool: load the GGUF once, then spawn `n_workers` contexts (each on
/// its own worker thread). Compute threads are split across workers so the
/// machine isn't oversubscribed. Returns the pool, whose `info()` is the model
/// metadata.
fn build_local_pool(
    path: &std::path::Path,
    params: &ModelInitParams,
    event_tx: Sender<WorkerEvent>,
    app: &AppHandle,
) -> Result<EnginePool, String> {
    let workers = params.n_workers.unwrap_or(2).clamp(1, 8) as usize;
    let app_progress = app.clone();
    let mut last_progress = 0.0f32;
    let model = engine::load_model_with_progress(path, params, move |progress: f32| {
        if progress - last_progress >= 0.01 || progress >= 1.0 {
            last_progress = progress;
            let _ = app_progress.emit_to(
                "main",
                "model-load-progress",
                LoadProgressEvent {
                    stage: "load",
                    progress,
                },
            );
        }
        true
    })?;
    let total_threads = params.n_threads.unwrap_or_else(|| {
        std::thread::available_parallelism()
            .map(|n| n.get() as u32)
            .unwrap_or(4)
    }) as i32;
    let per_threads = (total_threads as usize / workers).max(1) as i32;

    let factory = || {
        model
            .new_engine_with_threads(per_threads)
            .map(|e| Box::new(LocalGenerator::new(e)) as Box<dyn TextGenerator>)
    };
    let pool = EnginePool::spawn_with(factory, event_tx, workers)?;
    Ok(pool)
}

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

    let params = params.unwrap_or_default();
    let params_clone = params.clone();

    // Make sure the frontend learns of the full path the user picked.
    let path_for_event = path.clone();
    let app_for_load = app.clone();
    let event_tx = state
        .worker_tx
        .lock()
        .await
        .clone()
        .unwrap_or_else(|| spawn_emitter(app.clone()));

    let pool = tokio::task::spawn_blocking(move || {
        build_local_pool(&path, &params_clone, event_tx, &app_for_load)
    })
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

    let info = pool.info();
    let mut pool_guard = state.pool.lock().await;
    let mut info_guard = state.info.lock().await;
    *pool_guard = Some(Arc::new(pool));
    *info_guard = Some(info.clone());

    // Align the eviction engine's budget with the loaded model's context.
    context_state.inner.lock().await.set_limit(info.context_size as usize);

    let _ = app.emit_to("main", "model-load-progress", LoadProgressEvent { stage: "done", progress: 1.0 });
    let _ = app.emit("model-loaded", &info);
    let _ = app.emit("model-path", &path_for_event);
    drop(info_guard);
    drop(pool_guard);
    Ok(Some(info))
}

/// Fetch the available model ids for a remote provider so the connection UI
/// can offer them as a dropdown (see `remote::list_models`).
#[tauri::command]
async fn list_remote_models(config: RemoteModelConfig) -> Result<Vec<String>, String> {
    remote::list_models(&config).await
}

/// Configure and activate the remote (OpenAI-compatible) backend. Swaps out
/// any local pool, aligns the context budget, and fires the same
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
    // Remote backends are cheap to duplicate: give each worker its own client.
    let workers = 4usize;
    let event_tx = state
        .worker_tx
        .lock()
        .await
        .clone()
        .unwrap_or_else(|| spawn_emitter(app.clone()));
    let factory = || {
        RemoteGenerator::new(config.clone())
            .map(|g| Box::new(g) as Box<dyn TextGenerator>)
    };
    let pool = EnginePool::spawn_with(factory, event_tx, workers)?;
    let info = pool.info();

    let mut pool_guard = state.pool.lock().await;
    let mut info_guard = state.info.lock().await;
    *pool_guard = Some(Arc::new(pool));
    *info_guard = Some(info.clone());
    context_state
        .inner
        .lock()
        .await
        .set_limit(info.context_size as usize);
    drop(info_guard);
    drop(pool_guard);

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
    let mut pool_guard = state.pool.lock().await;
    let mut info_guard = state.info.lock().await;
    *pool_guard = None;
    *info_guard = None;
    drop(info_guard);
    drop(pool_guard);
    Ok(())
}

/// Poll the current model metadata (used on webview startup / reconnect).
#[tauri::command]
async fn model_status(state: State<'_, InferenceState>) -> Result<Option<ModelInfo>, String> {
    let info_guard = state.info.lock().await;
    Ok(info_guard.clone())
}

/// Streaming inference. Dispatches to a worker in the pool (round-robin); the
/// worker's own thread holds the generator for the whole generation. A fresh
/// circuit-breaker token is armed per run so an old cancellation can never leak
/// into this one.
#[tauri::command]
async fn stream_inference(
    app: AppHandle,
    state: State<'_, InferenceState>,
    interrupt_state: State<'_, InterruptState>,
    request: InferenceRequest,
) -> Result<u64, String> {
    let pool = state.pool.lock().await.clone().ok_or("No model loaded")?;

    let session_id = interrupt_state.next_session();
    let interrupt = interrupt_state.arm();
    let _ = app.emit("inference-started", StartedEvent { session_id });

    let tx = state
        .worker_tx
        .lock()
        .await
        .clone()
        .unwrap_or_else(|| spawn_emitter(app.clone()));

    let mut gen = pool.handle(0);
    let tx_clone = tx.clone();

    // ---- Greeting shortcut for plain chat (non-agent) mode.
    // Tiny models hallucinate random content for greetings.  Return a canned
    // reply so the user gets something sensible without burning tokens.
    {
        let prompt_lower = request.prompt.trim().to_ascii_lowercase();
        let bare = prompt_lower.trim_end_matches(['!', '.', '?', ',', ';', ':']);
        let is_greet = matches!(
            bare,
            "hi" | "hello" | "hey" | "howdy" | "sup" | "yo" | "hiya"
                | "thanks" | "thank you" | "ty" | "thx" | "cheers"
                | "bye" | "goodbye" | "see you" | "see ya"
        ) || (bare.split_whitespace().count() <= 2 && bare.len() <= 20);
        if is_greet {
            let reply = if bare.starts_with("bye") || bare == "goodbye" || bare == "see you" || bare == "see ya" {
                "Goodbye! Feel free to come back anytime.".to_string()
            } else if bare.starts_with("thank") || bare == "ty" || bare == "thx" || bare == "cheers" {
                "You're welcome! Let me know if you need anything else.".to_string()
            } else {
                "Hi there! I'm your AI coding assistant. I can help you explore, edit, test, and fix your codebase. What would you like to work on?".to_string()
            };
            let _ = tx_clone.send(WorkerEvent::Token { session_id, delta: reply.clone() });
            let _ = tx_clone.send(WorkerEvent::Done {
                session_id,
                done: engine::InferenceDone {
                    total_tokens: 0,
                    generated_chars: reply.len() as u64,
                    tokens_per_sec: 0.0,
                    elapsed_ms: 0,
                    stop_reason: "done".into(),
                    outcome: "completed".into(),
                    input_tokens: 0,
                    output_tokens: 0,
                    cache_read_tokens: 0,
                    cache_write_tokens: 0,
                    reasoning_tokens: 0,
                },
            });
            return Ok(session_id);
        }
    }

    std::thread::spawn(move || {
        let result = gen.generate(&request, session_id, &interrupt, &tx_clone);
        let _ = tx_clone.send(match result {
            Ok(outcome) => WorkerEvent::Done { session_id, done: outcome.done },
            Err(message) => WorkerEvent::Error { session_id, message },
        });
    });

    Ok(session_id)
}

/// Agentic task execution: an orchestrated generate → tool-call → feedback
/// loop (see `agent::orchestrator`). The whole loop runs on one native thread;
/// every generation dispatches to an engine-pool worker, and a decomposed task
/// with spare workers runs its subtasks concurrently. The circuit breaker token
/// is armed once and shared with both generation and tool sub-processes.
#[tauri::command]
async fn agent_run_task(
    app: AppHandle,
    state: State<'_, InferenceState>,
    interrupt_state: State<'_, InterruptState>,
    context_state: State<'_, ContextState>,
    tool_state: State<'_, ToolState>,
    request: agent::orchestrator::AgentTaskRequest,
) -> Result<u64, String> {
    let pool = state.pool.lock().await.clone().ok_or("No model loaded")?;

    let session_id = interrupt_state.next_session();
    let interrupt = interrupt_state.arm();
    let _ = app.emit("inference-started", StartedEvent { session_id });

    let tx = state
        .worker_tx
        .lock()
        .await
        .clone()
        .unwrap_or_else(|| spawn_emitter(app.clone()));

    // SAFETY: `ToolState` is Tauri-managed and outlives the app; the worker
    // thread(s) only read its workspace root + MCP cache, all behind mutexes.
    let tool_state_ref: &'static ToolState = unsafe {
        std::mem::transmute::<&ToolState, &'static ToolState>(tool_state.inner())
    };

    let context_snapshot = context_state.inner.lock().await.messages();
    let app_for_thread = app.clone();
    let tx_clone = tx.clone();
    let context_budget = pool.info().context_size as usize;
    let pool_for_thread = pool.clone();

    std::thread::spawn(move || {
        let result = agent::orchestrator::run_agent_loop_pool(
            &pool_for_thread,
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
async fn pick_text_file(app: AppHandle) -> Result<Option<String>, String> {
    let picked = app
        .dialog()
        .file()
        .set_title("Open file")
        .add_filter("Text files", &[
            "txt", "md", "json", "jsonc", "ts", "tsx", "js", "jsx", "css", "scss",
            "html", "htm", "xml", "yaml", "yml", "toml", "ini", "cfg", "conf",
            "py", "rs", "go", "java", "c", "cpp", "h", "hpp", "cs", "rb",
            "php", "sh", "bash", "zsh", "fish", "ps1", "bat", "cmd",
            "sql", "graphql", "gql", "proto", "lua", "r", "swift", "kt",
            "vue", "svelte", "astro", "wxml", "wxss",
        ])
        .blocking_pick_file();
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

/// Complete a pending permission-approval request from the UI. The decision
/// selects the remembered scope: `allow_once`, `allow_session`,
/// `always_allow` or `deny` (anything else is treated as a deny).
#[tauri::command]
async fn agent_respond_permission(
    state: State<'_, ToolState>,
    request_id: String,
    decision: String,
) -> Result<(), String> {
    let decision = match decision.as_str() {
        "allow_once" => PermissionDecision::AllowOnce,
        "allow_session" => PermissionDecision::AllowSession,
        "always_allow" => PermissionDecision::AlwaysAllow,
        _ => PermissionDecision::Deny,
    };
    let mut reqs = state.permission_requests.lock().await;
    if let Some(tx) = reqs.remove(&request_id) {
        let _ = tx.send(decision);
    }
    Ok(())
}

/// Recent audit-log entries (`{workspace}/.ai/audit.jsonl`), newest first.
#[tauri::command]
async fn agent_audit_log(
    state: State<'_, ToolState>,
    limit: Option<usize>,
) -> Result<Vec<Value>, String> {
    let ws = state.workspace.lock().await.clone();
    let Some(ws) = ws else { return Ok(Vec::new()) };
    let path = ws.join(".ai/audit.jsonl");
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("Failed to read the audit log: {e}"))?;
    let mut entries: Vec<Value> = text
        .lines()
        .rev()
        .filter_map(|l| serde_json::from_str(l.trim_end()).ok())
        .collect();
    let limit = limit.unwrap_or(100).clamp(1, 500);
    entries.truncate(limit);
    Ok(entries)
}

/// Effective policy snapshot for the UI.
#[tauri::command]
async fn agent_policy_snapshot(state: State<'_, ToolState>) -> Result<Value, String> {
    let ws = state.workspace.lock().await.clone();
    Ok(agent::policy::snapshot(ws.as_deref()))
}

/// Create a git checkpoint commit directly from the UI (bypasses the agent
/// tool loop; safe, additive — `git add -A` + `git commit`).
#[tauri::command]
async fn agent_git_checkpoint_cmd(
    state: State<'_, ToolState>,
    interrupt_state: State<'_, InterruptState>,
    message: Option<String>,
) -> Result<agent::ToolResult, String> {
    agent::tools::git_checkpoint(&state, &interrupt_state.current(), message.as_deref()).await
}

/// List existing checkpoints (hash/subject/relative age), newest first, for
/// the one-click revert UI.
#[tauri::command]
async fn agent_git_checkpoints_cmd(
    state: State<'_, ToolState>,
    interrupt_state: State<'_, InterruptState>,
) -> Result<Vec<Value>, String> {
    agent::tools::git_checkpoints(&state, &interrupt_state.current())
        .await
        .map(|v| v.into_iter().map(Value::from).collect())
}

/// Hard-reset to a checkpoint commit. Destructive by nature — the frontend
/// confirms before calling; the workspace policy's `git_revert` ask-rule does
/// not apply here because this is an explicit user action, not a model call.
#[tauri::command]
async fn agent_git_revert_cmd(
    state: State<'_, ToolState>,
    interrupt_state: State<'_, InterruptState>,
    commit: Option<String>,
) -> Result<agent::ToolResult, String> {
    agent::tools::git_revert(&state, &interrupt_state.current(), commit.as_deref()).await
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
            pick_text_file,
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
            agent_audit_log,
            agent_policy_snapshot,
            agent_git_checkpoint_cmd,
            agent_git_checkpoints_cmd,
            agent_git_revert_cmd,
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

