// Prevents an extra console window on Windows in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod agent;
mod api_server;
mod engine;
mod hub;
mod logging;
mod remote;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

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
use tokio_util::sync::CancellationToken;

/// Shared inference state. The engine lives as an [`EnginePool`] — one worker
/// thread per generator, each owning its context/client for its whole life.
/// Model load (blocking) runs in `spawn_blocking`; generations dispatch to
/// workers over channels, so there is no `'static` transmute of the engine and
/// several generations can run concurrently (parallel sub-tasks).
struct InferenceState {
    pool: Mutex<Option<Arc<EnginePool>>>,
    info: Mutex<Option<ModelInfo>>,
    /// Absolute path of the GGUF currently loaded (shown next to the
    /// Load/Unload button); `None` when nothing is loaded.
    loaded_path: Mutex<Option<String>>,
    worker_tx: Mutex<Option<crossbeam_channel::Sender<WorkerEvent>>>,
}

impl Default for InferenceState {
    fn default() -> Self {
        Self {
            pool: Mutex::new(None),
            info: Mutex::new(None),
            loaded_path: Mutex::new(None),
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

/// Hugging Face download registry: one cancellation token per active
/// `{repo}::{file}` download so the UI (or a second call) can cancel it.
struct HubState {
    active: Arc<Mutex<HashMap<String, CancellationToken>>>,
}

impl Default for HubState {
    fn default() -> Self {
        Self {
            active: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

/// Local OpenAI-compatible API server state: the shared engine handle the
/// server thread reads from + the optional running listener.
struct ApiServerState {
    engine: api_server::SharedEngine,
    server: Mutex<Option<api_server::ApiServerHandle>>,
}

impl Default for ApiServerState {
    fn default() -> Self {
        Self {
            engine: api_server::SharedEngine::default(),
            server: Mutex::new(None),
        }
    }
}

/// Frontend -> worker bridge. This channel is *bounded*: llama.cpp decode
/// pauses when the webview cannot keep up, which is exactly the backpressure
/// we want. The receiving loop forwards every event to the emitter task.
///
/// This loop is also the console-log observer: it mirrors every lifecycle
/// event (first token, throttled streaming progress, steps, subtasks, tool
/// results, completion stats, errors) to stderr so `tauri dev` / a terminal
/// launch shows live LLM progress without touching the engine or UI code.
fn spawn_emitter(app: AppHandle) -> crossbeam_channel::Sender<WorkerEvent> {
    let (tx, rx) = bounded::<WorkerEvent>(256);
    let app_sink = app.clone();
    std::thread::spawn(move || {
        let mut streams: HashMap<u64, logging::StreamProgress> = HashMap::new();
        while let Ok(event) = rx.recv() {
            match event {
                WorkerEvent::Token { session_id, delta } => {
                    let chars = delta.chars().count() as u64;
                    let progress = streams.entry(session_id).or_default();
                    if let Some(line) = progress.record(chars) {
                        logging::info(Some(session_id), "llm.stream", &line);
                    }
                    let _ = app_sink.emit("inference-token", TokenEvent { session_id, delta });
                }
                WorkerEvent::Step { session_id, step } => {
                    logging::info(
                        Some(session_id),
                        "llm.step",
                        &format!(
                            "step {} · {} · {} tok · {} ms · {} tool call(s)",
                            step.step, step.group, step.tokens, step.elapsed_ms, step.tool_calls
                        ),
                    );
                    let _ = app_sink.emit("agent-step", StepEvent { session_id, step });
                }
                WorkerEvent::Subtask {
                    session_id,
                    subtask,
                } => {
                    logging::info(
                        Some(session_id),
                        "llm.subtask",
                        &format!(
                            "subtask {}/{} `{}` {}",
                            subtask.index, subtask.total, subtask.title, subtask.status
                        ),
                    );
                    let _ = app_sink.emit(
                        "agent-subtask",
                        SubtaskEvent {
                            session_id,
                            subtask,
                        },
                    );
                }
                WorkerEvent::Done { session_id, done } => {
                    streams.remove(&session_id);
                    logging::info(
                        Some(session_id),
                        "llm.done",
                        &format!(
                            "{} in {:.1} s — out={} tok ({:.1} tok/s) in={} cache_r={} cache_w={} reasoning={}",
                            done.outcome,
                            done.elapsed_ms as f64 / 1000.0,
                            done.output_tokens,
                            done.tokens_per_sec,
                            done.input_tokens,
                            done.cache_read_tokens,
                            done.cache_write_tokens,
                            done.reasoning_tokens
                        ),
                    );
                    let _ = app_sink.emit("inference-done", DoneEvent { session_id, done });
                }
                WorkerEvent::Error {
                    session_id,
                    message,
                } => {
                    streams.remove(&session_id);
                    logging::error(
                        Some(session_id),
                        "llm.error",
                        &logging::preview(&message, 300),
                    );
                    let _ = app_sink.emit(
                        "inference-error",
                        ErrorEvent {
                            session_id,
                            message,
                        },
                    );
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

/// Load a local GGUF file into the pool, align the context budget and fire
/// the standard `model-loaded` event flow. Shared by the file-picker flow and
/// the hub "load downloaded model" flow.
async fn install_local_model(
    app: AppHandle,
    inference: &InferenceState,
    context_state: &ContextState,
    api: Option<&ApiServerState>,
    path: PathBuf,
    params: ModelInitParams,
) -> Result<ModelInfo, String> {
    let event_tx = inference
        .worker_tx
        .lock()
        .await
        .clone()
        .unwrap_or_else(|| spawn_emitter(app.clone()));

    let load_started = std::time::Instant::now();
    logging::info(None, "model", &format!("loading {} …", path.display()));
    let app_for_load = app.clone();
    let path_for_event = path.clone();
    let path_for_state = path.display().to_string();
    let pool = tokio::task::spawn_blocking(move || {
        build_local_pool(&path, &params, event_tx, &app_for_load)
    })
    .await
    .map_err(|e| format!("Model load task panicked: {e}"))?
    .inspect_err(|_| {
        let _ = app.emit_to(
            "main",
            "model-load-progress",
            LoadProgressEvent {
                stage: "error",
                progress: 0.0,
            },
        );
    })?;

    let info = pool.info();
    logging::info(
        None,
        "model",
        &format!(
            "loaded {} in {:.1}s",
            info.name,
            load_started.elapsed().as_secs_f32()
        ),
    );
    *inference.loaded_path.lock().await = Some(path_for_state);
    let mut pool_guard = inference.pool.lock().await;
    let mut info_guard = inference.info.lock().await;
    *pool_guard = Some(Arc::new(pool));
    *info_guard = Some(info.clone());
    drop(pool_guard);
    drop(info_guard);

    // Align the eviction engine's budget with the loaded model's context.
    context_state
        .inner
        .lock()
        .await
        .set_limit(info.context_size as usize);

    // Keep the API server's view of the engine in sync.
    if let Some(api) = api {
        let shared_pool = inference.pool.lock().await.clone();
        api.engine.set(shared_pool);
    }

    let _ = app.emit_to(
        "main",
        "model-load-progress",
        LoadProgressEvent {
            stage: "done",
            progress: 1.0,
        },
    );
    let _ = app.emit("model-loaded", &info);
    let _ = app.emit("model-path", &path_for_event);
    persist_model_path(&app, &path_for_event);
    Ok(info)
}

/// Pick a GGUF model file, load it and return its metadata. Blocking model
/// load is moved to `spawn_blocking` so the webview stays responsive.
#[tauri::command]
async fn pick_and_load_model(
    app: AppHandle,
    state: State<'_, InferenceState>,
    context_state: State<'_, ContextState>,
    api_state: State<'_, ApiServerState>,
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

    let info = install_local_model(
        app,
        &state,
        &context_state,
        Some(&api_state),
        path,
        params.unwrap_or_default(),
    )
    .await?;
    Ok(Some(info))
}

/// Load a GGUF from an explicit path (used for models downloaded via the hub).
#[tauri::command]
async fn load_model_from_path(
    app: AppHandle,
    state: State<'_, InferenceState>,
    context_state: State<'_, ContextState>,
    api_state: State<'_, ApiServerState>,
    path: String,
    params: Option<ModelInitParams>,
) -> Result<ModelInfo, String> {
    let p = PathBuf::from(&path);
    if !p.is_file() {
        return Err(format!("Not a file: {}", p.display()));
    }
    install_local_model(
        app,
        &state,
        &context_state,
        Some(&api_state),
        p,
        params.unwrap_or_default(),
    )
    .await
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
    api_state: State<'_, ApiServerState>,
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
    let factory =
        || RemoteGenerator::new(config.clone()).map(|g| Box::new(g) as Box<dyn TextGenerator>);
    let pool = EnginePool::spawn_with(factory, event_tx, workers)?;
    let info = pool.info();

    let mut pool_guard = state.pool.lock().await;
    let mut info_guard = state.info.lock().await;
    *pool_guard = Some(Arc::new(pool));
    *info_guard = Some(info.clone());
    drop(pool_guard);
    drop(info_guard);
    api_state.engine.set(state.pool.lock().await.clone());
    context_state
        .inner
        .lock()
        .await
        .set_limit(info.context_size as usize);

    let _ = app.emit_to(
        "main",
        "model-load-progress",
        LoadProgressEvent {
            stage: "done",
            progress: 1.0,
        },
    );
    let _ = app.emit("model-loaded", &info);
    Ok(info)
}

#[tauri::command]
async fn unload_model(
    state: State<'_, InferenceState>,
    interrupt_state: State<'_, InterruptState>,
    api_state: State<'_, ApiServerState>,
) -> Result<(), String> {
    interrupt_state.trigger();
    logging::info(None, "model", "unloading model …");
    let mut pool_guard = state.pool.lock().await;
    let mut info_guard = state.info.lock().await;
    *pool_guard = None;
    *info_guard = None;
    drop(info_guard);
    drop(pool_guard);
    api_state.engine.set(None);
    *state.loaded_path.lock().await = None;
    logging::info(None, "model", "model unloaded");
    Ok(())
}

/// Poll the current model metadata (used on webview startup / reconnect).
#[tauri::command]
async fn model_status(state: State<'_, InferenceState>) -> Result<Option<ModelInfo>, String> {
    let info_guard = state.info.lock().await;
    Ok(info_guard.clone())
}

/// Absolute GGUF path currently loaded, for display next to Load/Unload.
#[tauri::command]
async fn loaded_model_path(state: State<'_, InferenceState>) -> Result<Option<String>, String> {
    Ok(state.loaded_path.lock().await.clone())
}

// ---------------------------------------------------------------------------
// Hugging Face hub
// ---------------------------------------------------------------------------

/// Search the hub for GGUF repos (public models, no auth).
#[tauri::command]
async fn hf_search(query: String, limit: Option<usize>) -> Result<Vec<hub::HfModel>, String> {
    hub::search(&query, limit.unwrap_or(20)).await
}

/// Start streaming a GGUF from the hub into `{app_data}/models/…`. Progress
/// arrives on the `hf-download-progress` event channel; one download per
/// repo+file at a time.
#[tauri::command]
async fn hf_download_model(
    app: AppHandle,
    hub_state: State<'_, HubState>,
    repo_id: String,
    file_name: String,
) -> Result<(), String> {
    let key = format!("{repo_id}::{file_name}");
    {
        let mut active = hub_state.active.lock().await;
        if active.contains_key(&key) {
            return Err("Download already in progress".to_string());
        }
        active.insert(key.clone(), CancellationToken::new());
    }
    let token = hub_state.active.lock().await.get(&key).cloned().unwrap();
    let registry = hub_state.active.clone();

    let models_dir = app_data_dir(&app).join("models");
    let app_sink = app.clone();
    let repo = repo_id.clone();
    let file = file_name.clone();
    tokio::spawn(async move {
        let result = hub::download_file(&models_dir, &repo, &file, &token, |p| {
            let _ = app_sink.emit(
                "hf-download-progress",
                json!({
                    "repoId": repo,
                    "file": file,
                    "receivedBytes": p.received,
                    "totalBytes": p.total,
                    "done": p.done,
                }),
            );
        })
        .await;
        registry.lock().await.remove(&key);
        match result {
            Ok(_) => {}
            Err(e) if e == "__cancelled__" => {
                let _ = app.emit(
                    "hf-download-progress",
                    json!({ "repoId": repo, "file": file, "cancelled": true }),
                );
            }
            Err(e) => {
                let _ = app.emit(
                    "hf-download-progress",
                    json!({ "repoId": repo, "file": file, "error": e }),
                );
            }
        }
    });
    Ok(())
}

/// Cancel an in-progress hub download.
#[tauri::command]
async fn hf_cancel_download(
    hub_state: State<'_, HubState>,
    repo_id: String,
    file_name: String,
) -> Result<(), String> {
    if let Some(token) = hub_state
        .active
        .lock()
        .await
        .get(&format!("{repo_id}::{file_name}"))
    {
        token.cancel();
    }
    Ok(())
}

/// GGUF files already downloaded through the hub.
#[tauri::command]
async fn list_downloaded_models(app: AppHandle) -> Result<Vec<hub::DownloadedModel>, String> {
    Ok(hub::list_downloaded(&app_data_dir(&app).join("models")))
}

/// Replay the recent log lines so the webview Console window shows history
/// captured before its listeners attached (startup, model auto-load, …).
#[tauri::command]
async fn console_history() -> Result<Vec<String>, String> {
    Ok(logging::recent_lines())
}

/// Remember the last successfully loaded GGUF so future launches can restore
/// it without re-picking (read-modify-write of `settings.json`; best-effort —
/// a failure must never fail the model load itself).
fn persist_model_path(app: &AppHandle, path: &Path) {
    let dir = app_data_dir(app);
    let file = dir.join("settings.json");
    let mut settings: Value = std::fs::read_to_string(&file)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_else(|| json!({}));
    let as_str = path.display().to_string();
    if settings.get("modelPath").and_then(Value::as_str) == Some(as_str.as_str()) {
        return;
    }
    settings["modelPath"] = json!(as_str);
    if std::fs::create_dir_all(&dir).is_ok() {
        if let Ok(text) = serde_json::to_string_pretty(&settings) {
            if std::fs::write(&file, text).is_ok() {
                logging::info(None, "model", "saved default model path");
            }
        }
    }
}

/// Startup convenience: load a model without user interaction.
///
/// Resolution order:
///   1. `modelPath` persisted in `settings.json` (last successful load),
///   2. any `*.gguf` previously downloaded through the HF browser,
///   3. any `*.gguf` directly inside a `models/` folder next to the working
///      directory (the dev-checkout layout, e.g. `D:\ai\models`).
///
/// Returns `Ok(None)` when nothing suitable exists — that is not an error,
/// just a first-run state where the user still picks a model manually.
#[tauri::command]
async fn auto_load_model(
    app: AppHandle,
    state: State<'_, InferenceState>,
    context_state: State<'_, ContextState>,
    api_state: State<'_, ApiServerState>,
) -> Result<Option<ModelInfo>, String> {
    if state.pool.lock().await.is_some() {
        return Ok(state.info.lock().await.clone());
    }

    let mut candidates: Vec<PathBuf> = Vec::new();
    let saved: Option<String> = {
        let file = app_data_dir(&app).join("settings.json");
        std::fs::read_to_string(&file)
            .ok()
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
            .and_then(|v| v.get("modelPath").and_then(Value::as_str).map(String::from))
    };
    if let Some(p) = saved {
        candidates.push(PathBuf::from(p));
    }
    for m in hub::list_downloaded(&app_data_dir(&app).join("models")) {
        candidates.push(PathBuf::from(m.path));
    }
    if let Ok(entries) = std::fs::read_dir(Path::new("models")) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.extension().and_then(|e| e.to_str()) == Some("gguf") {
                candidates.push(p);
            }
        }
    }

    for path in candidates {
        if !path.is_file() {
            continue;
        }
        logging::info(None, "model", &format!("auto-loading {}", path.display()));
        return install_local_model(
            app,
            &state,
            &context_state,
            Some(&api_state),
            path,
            ModelInitParams::default(),
        )
        .await
        .map(Some);
    }
    logging::warn(None, "model", "no local GGUF found to auto-load");
    Ok(None)
}

// ---------------------------------------------------------------------------
// Local OpenAI-compatible API server
// ---------------------------------------------------------------------------

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ApiServerStatus {
    running: bool,
    port: Option<u16>,
}

/// Start the loopback-only OpenAI-compatible server (idempotent).
#[tauri::command]
async fn api_server_start(
    state: State<'_, ApiServerState>,
    port: Option<u16>,
) -> Result<ApiServerStatus, String> {
    let mut guard = state.server.lock().await;
    if let Some(handle) = guard.as_ref() {
        return Ok(ApiServerStatus {
            running: true,
            port: Some(handle.port),
        });
    }
    let handle = api_server::start(state.engine.clone(), port.unwrap_or(8080))?;
    let port = handle.port;
    *guard = Some(handle);
    Ok(ApiServerStatus {
        running: true,
        port: Some(port),
    })
}

/// Stop the server (no-op when not running).
#[tauri::command]
async fn api_server_stop(state: State<'_, ApiServerState>) -> Result<ApiServerStatus, String> {
    let mut guard = state.server.lock().await;
    *guard = None; // dropping the handle closes the listener
    Ok(ApiServerStatus {
        running: false,
        port: None,
    })
}

/// Current server status.
#[tauri::command]
async fn api_server_status(state: State<'_, ApiServerState>) -> Result<ApiServerStatus, String> {
    let guard = state.server.lock().await;
    Ok(match guard.as_ref() {
        Some(h) => ApiServerStatus {
            running: true,
            port: Some(h.port),
        },
        None => ApiServerStatus {
            running: false,
            port: None,
        },
    })
}

// ---------------------------------------------------------------------------
// RAG attachments (UI paperclip flow shares the agent's index)
// ---------------------------------------------------------------------------

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct AttachedFileInfo {
    path: String,
    bytes: u64,
    // Serialized as `chunkCount` via rename_all above.
    chunk_count: usize,
}

/// Attach a file from the UI into the same session index used by the agent.
#[tauri::command]
async fn agent_attach_file(
    state: State<'_, ToolState>,
    path: String,
) -> Result<AttachedFileInfo, String> {
    let text = std::fs::read_to_string(&path).map_err(|e| format!("Failed to read: {e}"))?;
    let file = { state.rag.lock().unwrap().attach(&path, &text)? };
    Ok(AttachedFileInfo {
        path,
        bytes: file.bytes,
        chunk_count: file.chunk_count,
    })
}

/// Detach a file from the session index (UI chip ✕).
#[tauri::command]
async fn agent_detach_file(state: State<'_, ToolState>, path: String) -> Result<(), String> {
    if !state.rag.lock().unwrap().detach(&path) {
        return Err(format!("`{path}` is not attached"));
    }
    Ok(())
}

/// List currently attached files.
#[tauri::command]
async fn agent_list_attachments(
    state: State<'_, ToolState>,
) -> Result<Vec<AttachedFileInfo>, String> {
    let rag = state.rag.lock().unwrap();
    Ok(rag
        .list()
        .iter()
        .map(|f| AttachedFileInfo {
            path: f.path.clone(),
            bytes: f.bytes,
            chunk_count: f.chunk_count,
        })
        .collect())
}

/// UI dictation: persist a webview-recorded audio blob and transcribe it with
/// the same whisper pipeline the `transcribe_audio` tool uses.
#[tauri::command]
async fn voice_transcribe_data(
    app: AppHandle,
    state: State<'_, ToolState>,
    data: Vec<u8>,
    ext: Option<String>,
) -> Result<String, String> {
    let dir = app_data_dir(&app).join("voice");
    std::fs::create_dir_all(&dir).map_err(|e| format!("mkdir failed: {e}"))?;
    let safe_ext = ext
        .unwrap_or_else(|| "webm".into())
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(8)
        .collect::<String>();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let path = dir.join(format!("dictation-{nanos}.{safe_ext}"));
    std::fs::write(&path, &data).map_err(|e| format!("Write failed: {e}"))?;
    agent::tools::transcribe_file(&state, &path).await
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
    logging::info(
        Some(session_id),
        "llm.request",
        &format!(
            "chat request · {} chars · max_tokens={} temp={:.2} top_p={:.2}{}",
            request.prompt.chars().count(),
            request.max_tokens,
            request.temperature.unwrap_or(0.0),
            request.top_p.unwrap_or(0.0),
            if logging::prompt_preview_enabled() {
                format!(" · prompt: {}", logging::preview(&request.prompt, 80))
            } else {
                String::new()
            }
        ),
    );

    let tx = state
        .worker_tx
        .lock()
        .await
        .clone()
        .unwrap_or_else(|| spawn_emitter(app.clone()));

    let mut gen = pool.handle(0);
    let tx_clone = tx.clone();

    std::thread::spawn(move || {
        let result = gen.generate(&request, session_id, &interrupt, &tx_clone);
        // Guard against silent empty generations: a completed run with zero
        // output would leave the chat bubble empty and look like a hang.
        if let Ok(outcome) = &result {
            if outcome.done.generated_chars == 0 && outcome.done.stop_reason == "done" {
                logging::warn(
                    Some(session_id),
                    "llm.request",
                    "model returned an empty response",
                );
                let _ = tx_clone.send(WorkerEvent::Token {
                    session_id,
                    delta: "(the model returned an empty response — try rephrasing or loading a larger model)".to_string(),
                });
            }
        }
        let _ = tx_clone.send(match result {
            Ok(outcome) => WorkerEvent::Done {
                session_id,
                done: outcome.done,
            },
            Err(message) => WorkerEvent::Error {
                session_id,
                message,
            },
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
    logging::info(
        Some(session_id),
        "llm.request",
        &format!(
            "agent task · {} chars · max_steps={} plan_mode={} decompose={} verify={}{}",
            request.prompt.chars().count(),
            request.max_steps.unwrap_or(6),
            request.plan_mode,
            request.decompose,
            request.verify,
            if logging::prompt_preview_enabled() {
                format!(" · prompt: {}", logging::preview(&request.prompt, 80))
            } else {
                String::new()
            }
        ),
    );

    let tx = state
        .worker_tx
        .lock()
        .await
        .clone()
        .unwrap_or_else(|| spawn_emitter(app.clone()));

    // SAFETY: `ToolState` is Tauri-managed and outlives the app; the worker
    // thread(s) only read its workspace root + MCP cache, all behind mutexes.
    let tool_state_ref: &'static ToolState =
        unsafe { std::mem::transmute::<&ToolState, &'static ToolState>(tool_state.inner()) };

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
            Ok(outcome) => WorkerEvent::Done {
                session_id,
                done: outcome.done,
            },
            Err(message) => WorkerEvent::Error {
                session_id,
                message,
            },
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
        .add_filter(
            "Text files",
            &[
                "txt", "md", "json", "jsonc", "ts", "tsx", "js", "jsx", "css", "scss", "html",
                "htm", "xml", "yaml", "yml", "toml", "ini", "cfg", "conf", "py", "rs", "go",
                "java", "c", "cpp", "h", "hpp", "cs", "rb", "php", "sh", "bash", "zsh", "fish",
                "ps1", "bat", "cmd", "sql", "graphql", "gql", "proto", "lua", "r", "swift", "kt",
                "vue", "svelte", "astro", "wxml", "wxss",
            ],
        )
        .blocking_pick_file();
    Ok(picked
        .and_then(|p| p.into_path().ok())
        .map(|p| p.to_string_lossy().into_owned()))
}

#[tauri::command]
async fn list_directory(
    _app: AppHandle,
    root: String,
    relative: Option<String>,
) -> Result<Vec<FsEntry>, String> {
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
        entries.push(FsEntry {
            name,
            path: rel,
            is_dir,
        });
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
    tokio::fs::write(&path, content)
        .await
        .map_err(|e| e.to_string())?;
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
    tokio::fs::write(&path, content)
        .await
        .map_err(|e| e.to_string())?;
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
async fn agent_tool_schemas() -> Result<std::collections::HashMap<String, serde_json::Value>, String>
{
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

// ---- auto-update (opt-in via AI_EDITOR_UPDATER_PUBKEY) ----

/// True when this build ships a signing key, i.e. has a live release channel.
/// The frontend uses it to decide whether any update UI makes sense.
#[tauri::command]
async fn updater_configured() -> bool {
    std::env::var("AI_EDITOR_UPDATER_PUBKEY")
        .map(|v| !v.trim().is_empty())
        .unwrap_or(false)
}

fn updater_endpoints() -> Result<Vec<url::Url>, String> {
    let raw = std::env::var("AI_EDITOR_UPDATER_ENDPOINTS").unwrap_or_else(|_| {
        "https://releases.ai-editor.dev/{{target}}/{{arch}}/{{current_version}}".to_string()
    });
    raw.split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| {
            s.parse::<url::Url>()
                .map_err(|e| format!("bad updater endpoint `{s}`: {e}"))
        })
        .collect()
}

/// Probe the release feed. Returns the newest version string when an update
/// exists, None when current — or when updates aren't configured at all.
#[tauri::command]
async fn update_check(app: AppHandle) -> Result<Option<String>, String> {
    use tauri_plugin_updater::UpdaterExt;
    if !updater_configured().await {
        return Ok(None);
    }
    let updater = app
        .updater_builder()
        .endpoints(updater_endpoints()?)
        .map_err(|e| e.to_string())?
        .build()
        .map_err(|e| e.to_string())?;
    Ok(updater
        .check()
        .await
        .map_err(|e| e.to_string())?
        .map(|u| u.version))
}

/// Load the user's persisted MCP server catalog for the settings UI.
#[tauri::command]
async fn mcp_catalog_load(app: AppHandle) -> Result<Vec<agent::mcp::McpServerConfig>, String> {
    agent::mcp::load_catalog(&app_config_dir(&app))
}

/// Replace the user's MCP server catalog from the settings UI.
#[tauri::command]
async fn mcp_catalog_save(
    app: AppHandle,
    state: State<'_, ToolState>,
    servers: Vec<agent::mcp::McpServerConfig>,
) -> Result<(), String> {
    // Validate before writing: unique non-empty names, non-empty commands.
    let mut seen = std::collections::HashSet::new();
    for s in &servers {
        if s.name.trim().is_empty() {
            return Err("Server name must not be empty".into());
        }
        if !seen.insert(s.name.clone()) {
            return Err(format!("Duplicate server name `{}`", s.name));
        }
        if s.bin.trim().is_empty() {
            return Err(format!("Server `{}` needs a command", s.name));
        }
    }
    // Evict cached connections that no longer match a live config entry.
    let stale: Vec<String> = {
        let cache = state.mcp_servers.lock().await;
        cache
            .keys()
            .filter(|key| {
                !servers
                    .iter()
                    .any(|s| s.enabled && **key == format!("{} {}", s.bin, s.args.join(" ")))
            })
            .cloned()
            .collect()
    };
    {
        let mut cache = state.mcp_servers.lock().await;
        for key in stale {
            cache.remove(&key);
        }
    }
    agent::mcp::save_catalog(&app_config_dir(&app), &servers)
}

// ---------------------------------------------------------------------------
// Skills & Rules
// ---------------------------------------------------------------------------

pub(crate) fn app_config_dir(app: &AppHandle) -> PathBuf {
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
        guard
            .clone()
            .ok_or("No workspace set - open a workspace first")?
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

/// Install a skill from a `.md` file or a folder containing `SKILL.md`.
/// `global` targets the user-global skills dir; otherwise the workspace's
/// `.ai/skills` is used. Rescans and returns the fresh report.
#[tauri::command]
async fn skill_install(
    app: AppHandle,
    state: State<'_, ToolState>,
    context_state: State<'_, ContextState>,
    knowledge_state: State<'_, Arc<KnowledgeState>>,
    source: String,
    global: bool,
) -> Result<KnowledgeReport, String> {
    let ws = {
        let guard = state.workspace.lock().await;
        guard
            .clone()
            .ok_or("No workspace set - open a workspace first")?
    };
    let config_dir = app_config_dir(&app);
    KnowledgeState::install(&ws, &config_dir, Path::new(&source), global)?;
    knowledge_state.scan(&ws, &config_dir)?;
    sync_knowledge(&context_state, &knowledge_state).await;
    let _ = app.emit("agent-knowledge", knowledge_report(&knowledge_state));
    Ok(knowledge_report(&knowledge_state))
}

/// Delete a skill (file or folder) from disk. Rescans and returns the report.
#[tauri::command]
async fn skill_uninstall(
    app: AppHandle,
    state: State<'_, ToolState>,
    context_state: State<'_, ContextState>,
    knowledge_state: State<'_, Arc<KnowledgeState>>,
    name: String,
) -> Result<KnowledgeReport, String> {
    let ws = {
        let guard = state.workspace.lock().await;
        guard
            .clone()
            .ok_or("No workspace set - open a workspace first")?
    };
    let config_dir = app_config_dir(&app);
    knowledge_state.uninstall(&name)?;
    knowledge_state.scan(&ws, &config_dir)?;
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
    let text =
        std::fs::read_to_string(&path).map_err(|e| format!("Failed to read the audit log: {e}"))?;
    let mut entries: Vec<Value> = text
        .lines()
        .rev()
        .filter_map(|l| serde_json::from_str(l.trim_end()).ok())
        .collect();
    let limit = limit.unwrap_or(100).clamp(1, 500);
    entries.truncate(limit);
    Ok(entries)
}

/// Effective policy snapshot for the UI (includes session-only YOLO flag and
/// per-session path grants).
#[tauri::command]
async fn agent_policy_snapshot(state: State<'_, ToolState>) -> Result<Value, String> {
    let ws = state.workspace.lock().await.clone();
    let mut snap = agent::policy::snapshot(ws.as_deref());
    if let Some(obj) = snap.as_object_mut() {
        obj.insert(
            "yolo".into(),
            Value::Bool(state.yolo.load(std::sync::atomic::Ordering::SeqCst)),
        );
        let grants = state.path_grants.lock().unwrap();
        obj.insert(
            "pathGrants".into(),
            serde_json::to_value(grants.iter().collect::<Vec<_>>()).unwrap_or(Value::Array(vec![])),
        );
    }
    Ok(snap)
}

/// Toggle the YOLO sub-mode (Bionic §3.3): ROUTINE shell commands skip the
/// approval dialog; red-zone stays denied unconditionally. Session-only.
#[tauri::command]
async fn agent_set_yolo(state: State<'_, ToolState>, on: bool) -> Result<(), String> {
    state.yolo.store(on, std::sync::atomic::Ordering::SeqCst);
    Ok(())
}

/// Grant a per-session `{path, mode}` allowance for a path OUTSIDE the
/// workspace (Bionic §3.3). mode: "read" | "write". Session-only — never
/// persisted to disk.
#[tauri::command]
async fn agent_grant_path(
    state: State<'_, ToolState>,
    path: String,
    mode: String,
) -> Result<(), String> {
    if mode != "read" && mode != "write" {
        return Err("mode must be \"read\" or \"write\".".into());
    }
    let p = std::path::PathBuf::from(path.trim());
    if !p.is_absolute() {
        return Err("path must be absolute.".into());
    }
    let mut grants = state.path_grants.lock().unwrap();
    grants.retain(|g| g.path != p);
    grants.push(agent::PathGrant { path: p, mode });
    Ok(())
}

/// Revoke every grant covering `path` (exact match or descendant).
#[tauri::command]
async fn agent_revoke_path(state: State<'_, ToolState>, path: String) -> Result<(), String> {
    let p = std::path::PathBuf::from(path.trim());
    let mut grants = state.path_grants.lock().unwrap();
    grants.retain(|g| g.path != p && !g.path.starts_with(&p));
    Ok(())
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
        .map(|v| v.into_iter().collect())
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

/// Sanitize a project string into a filesystem-safe key. This is the legacy
/// scheme (separators become underscores) and must stay byte-stable: existing
/// `sessions/<key>.jsonl` logs keep working across upgrades.
fn session_key(project: &str) -> String {
    project
        .replace(['/', '\\', ':'], "_")
        .trim_matches('_')
        .to_string()
}

/// Sanitize a chat id into a file-name stem (alphanumerics, `-`, `_` only, so
/// named-chat files can never escape the sessions directory).
fn chat_key(chat_id: &str) -> String {
    let cleaned: String = chat_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = cleaned.trim_matches('_').to_string();
    if trimmed.is_empty() {
        "chat".to_string()
    } else {
        trimmed.chars().take(64).collect()
    }
}

/// Resolve the JSONL log file for a `{project, chat}` pair.
///
/// The default chat keeps the legacy flat layout (`<key>.jsonl`) so pre-BN-11
/// history is untouched; named chats live in a per-project subdirectory
/// (`<key>/<chat>.jsonl`). The subdirectory also disambiguates cleanly — the
/// flat key itself may contain `__` runs (`D:\ai` → `D__ai`), so a flat
/// `<key>__<chat>.jsonl` naming would be ambiguous.
fn session_file(dir: &Path, project: &str, chat_id: Option<&str>) -> PathBuf {
    let safe = session_key(project);
    match chat_id.map(str::trim).filter(|c| !c.is_empty()) {
        None => dir.join(format!("{safe}.jsonl")),
        Some(chat) => dir.join(&safe).join(format!("{}.jsonl", chat_key(chat))),
    }
}

/// Remember the original workspace path behind a sanitized key
/// (`sessions/projects.json`) so the projects tree can show real names.
fn remember_project_name(dir: &Path, key: &str, original: &str) {
    if key == original {
        return;
    }
    let idx_path = dir.join("projects.json");
    let mut map: std::collections::BTreeMap<String, String> = std::fs::read_to_string(&idx_path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    if map.get(key).map(String::as_str) != Some(original) {
        map.insert(key.to_string(), original.to_string());
        if let Ok(json) = serde_json::to_string_pretty(&map) {
            let _ = std::fs::write(idx_path, json);
        }
    }
}

/// Best-effort chat title for the projects tree: the first user message's
/// first line, truncated to 64 chars. Reads at most the first 8 KiB.
fn chat_title(path: &Path) -> Option<String> {
    use std::io::Read;
    let mut f = std::fs::File::open(path).ok()?;
    let mut buf = [0u8; 8192];
    let n = f.read(&mut buf).ok()?;
    for line in String::from_utf8_lossy(&buf[..n]).lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if v.get("role").and_then(Value::as_str) != Some("user") {
            continue;
        }
        let content = v.get("content").and_then(Value::as_str).unwrap_or("");
        let title: String = content
            .lines()
            .next()
            .unwrap_or("")
            .trim()
            .chars()
            .take(64)
            .collect();
        return if title.is_empty() { None } else { Some(title) };
    }
    None
}

fn modified_ms(path: &Path) -> u64 {
    path.metadata()
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn count_lines(path: &Path) -> usize {
    use std::io::{BufRead, BufReader};
    std::fs::File::open(path)
        .map(|f| BufReader::new(f).lines().count())
        .unwrap_or(0)
}

/// One chat under a project, as shown in the sidebar tree. `id` is empty for
/// the default (legacy) chat.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionChatInfo {
    id: String,
    title: String,
    updated_at_ms: u64,
    turns: usize,
}

/// One project with all of its chats (projects/chats sidebar tree).
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionProjectInfo {
    /// Filesystem-safe key; stable across launches.
    key: String,
    /// Original workspace path when known, else the sanitized key. Frontend
    /// calls use this string form (it sanitizes back to `key`).
    name: String,
    last_active_ms: u64,
    chats: Vec<SessionChatInfo>,
}

/// List every known project and its chats, newest activity first.
#[tauri::command]
async fn session_projects(app: AppHandle) -> Result<Vec<SessionProjectInfo>, String> {
    let dir = app_data_dir(&app).join("sessions");
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let names: std::collections::BTreeMap<String, String> =
        std::fs::read_to_string(dir.join("projects.json"))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
    let mut projects: Vec<SessionProjectInfo> = Vec::new();
    let entries = std::fs::read_dir(&dir).map_err(|e| e.to_string())?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() && path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            let key = stem.to_string();
            let name = names.get(&key).cloned().unwrap_or_else(|| key.clone());
            let updated = modified_ms(&path);
            let title = chat_title(&path).unwrap_or_else(|| "Default chat".to_string());
            if let Some(p) = projects.iter_mut().find(|p| p.key == key) {
                p.chats.push(SessionChatInfo {
                    id: String::new(),
                    title,
                    updated_at_ms: updated,
                    turns: count_lines(&path),
                });
                p.last_active_ms = p.last_active_ms.max(updated);
            } else {
                projects.push(SessionProjectInfo {
                    key,
                    name,
                    last_active_ms: updated,
                    chats: vec![SessionChatInfo {
                        id: String::new(),
                        title,
                        updated_at_ms: updated,
                        turns: count_lines(&path),
                    }],
                });
            }
        } else if path.is_dir() {
            let Some(key) = path.file_name().and_then(|s| s.to_str()) else {
                continue;
            };
            let key = key.to_string();
            let name = names.get(&key).cloned().unwrap_or_else(|| key.clone());
            let mut chats: Vec<SessionChatInfo> = Vec::new();
            let mut last = 0u64;
            if let Ok(kids) = std::fs::read_dir(&path) {
                for kid in kids.flatten() {
                    let kp = kid.path();
                    if !kp.is_file() || kp.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                        continue;
                    }
                    let Some(id) = kp.file_stem().and_then(|s| s.to_str()) else {
                        continue;
                    };
                    let id = id.to_string();
                    let updated = modified_ms(&kp);
                    last = last.max(updated);
                    chats.push(SessionChatInfo {
                        title: chat_title(&kp).unwrap_or_else(|| "Empty chat".to_string()),
                        id,
                        updated_at_ms: updated,
                        turns: count_lines(&kp),
                    });
                }
            }
            match projects.iter_mut().find(|p| p.key == key) {
                Some(p) => {
                    p.chats.extend(chats);
                    p.last_active_ms = p.last_active_ms.max(last);
                }
                None => projects.push(SessionProjectInfo {
                    key,
                    name,
                    last_active_ms: last,
                    chats,
                }),
            }
        }
    }
    for p in &mut projects {
        p.chats.sort_by_key(|c| std::cmp::Reverse(c.updated_at_ms));
    }
    projects.sort_by_key(|p| std::cmp::Reverse(p.last_active_ms));
    Ok(projects)
}

/// Delete a named chat's log. The default (legacy) chat cannot be deleted.
#[tauri::command]
async fn session_delete_chat(
    app: AppHandle,
    project: String,
    chat_id: String,
) -> Result<(), String> {
    let chat = chat_id.trim();
    if chat.is_empty() {
        return Err("The default chat cannot be deleted".into());
    }
    let dir = app_data_dir(&app).join("sessions");
    let file = session_file(&dir, &project, Some(chat));
    if file.exists() {
        std::fs::remove_file(&file).map_err(|e| e.to_string())?;
    }
    if let Some(parent) = file.parent() {
        if parent != dir && parent.is_dir() {
            let _ = std::fs::remove_dir(parent); // only removes when empty
        }
    }
    Ok(())
}

/// Append one conversation record to the project+chat JSONL session log.
#[tauri::command]
async fn session_append(
    app: AppHandle,
    project: String,
    record: Value,
    chat_id: Option<String>,
) -> Result<(), String> {
    let dir = app_data_dir(&app).join("sessions");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let safe = session_key(&project);
    remember_project_name(&dir, &safe, &project);
    let file = session_file(&dir, &project, chat_id.as_deref());
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
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

/// Load the full history for one project chat (all JSONL records in order).
#[tauri::command]
async fn session_load(
    app: AppHandle,
    project: String,
    chat_id: Option<String>,
) -> Result<Vec<Value>, String> {
    let dir = app_data_dir(&app).join("sessions");
    let file = session_file(&dir, &project, chat_id.as_deref());
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

/// Headless boot-smoke mode (CI / BN-12): with `AI_EDITOR_SMOKE=1` the app
/// boots for real (plugins, state, window, webview) and the frontend reports
/// a successful mount via `smoke_boot_ok`, which prints a marker and exits 0.
/// A watchdog thread fails the run if the webview never reports.
fn smoke_enabled() -> bool {
    std::env::var("AI_EDITOR_SMOKE")
        .map(|v| !v.trim().is_empty() && v.trim() != "0")
        .unwrap_or(false)
}

/// Probe used by the frontend to decide whether to report boot success.
#[tauri::command]
async fn smoke_active() -> bool {
    smoke_enabled()
}

/// Frontend ack: the webview mounted and React rendered. Exits green.
#[tauri::command]
async fn smoke_boot_ok(app: AppHandle) -> Result<(), String> {
    if smoke_enabled() {
        println!("AI_EDITOR_SMOKE_OK");
        app.exit(0);
    }
    Ok(())
}

/// Frontend boot-failure report: surfaces webview-side errors (module
/// evaluation crashes, render errors, failed invokes) on stderr so a headless
/// smoke run shows *why* boot never completed instead of just timing out.
#[tauri::command]
async fn smoke_fail(message: String) -> Result<(), String> {
    eprintln!("AI_EDITOR_SMOKE_FAIL: {message}");
    Ok(())
}

/// Show + focus the main window (tray menu / tray click).
fn show_main_window(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

/// Build the system-tray icon with a Show/Quit menu (BN-11).
fn build_tray(app: &tauri::App) -> Result<(), tauri::Error> {
    use tauri::menu::{Menu, MenuItem};
    use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
    let show = MenuItem::with_id(app, "show", "Show AI Editor", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit AI Editor", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &quit])?;
    let mut builder = TrayIconBuilder::with_id("ai-editor-tray")
        .tooltip("AI Editor")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, ev| match ev.id.as_ref() {
            "show" => show_main_window(app),
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, ev| {
            // Left click toggles the window back into view.
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = ev
            {
                show_main_window(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    builder.build(app)?;
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // One shared knowledge snapshot backs both the UI commands and the agent's
    // `read_skill` tool, so a scan is immediately visible to both sides.
    let knowledge: std::sync::Arc<KnowledgeState> = std::sync::Arc::new(KnowledgeState::default());
    // The updater is opt-in: the plugin is only registered when a minisign
    // public key is provided (release builds set AI_EDITOR_UPDATER_PUBKEY).
    // Without it the app simply runs with auto-update disabled — no dead
    // endpoints, no signature errors on developer machines. `update_check`
    // mirrors this guard so the frontend can probe support safely.
    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_process::init());
    let builder = match std::env::var("AI_EDITOR_UPDATER_PUBKEY") {
        Ok(pubkey) if !pubkey.trim().is_empty() => builder.plugin(
            tauri_plugin_updater::Builder::new()
                .pubkey(pubkey.trim())
                .build(),
        ),
        _ => builder,
    };
    builder
        .manage(InferenceState::default())
        .manage(HubState::default())
        .manage(ApiServerState::default())
        .manage(ToolState {
            knowledge: knowledge.clone(),
            ..ToolState::default()
        })
        .manage(InterruptState::default())
        .manage(ContextState::default())
        .manage(knowledge)
        .setup(|app| {
            // Route every [BE]/[LLM] log line into (a) the rolling file
            // appender under the app-data `logs/` dir and (b) the in-app
            // Console window via a `console-log` event to the webview.
            let handle = app.handle().clone();
            let log_dir = app_data_dir(&handle).join("logs");
            logging::init(
                log_dir,
                Box::new(move |line| {
                    let _ = handle.emit("console-log", line.to_string());
                }),
            );
            // Forward native llama.cpp/ggml logs (model load, KV cache,
            // backend init) into the same pipeline.
            engine::install_native_model_logs();
            build_tray(app)?;
            if smoke_enabled() {
                // Watchdog: if the webview never reports a successful boot
                // (blank page, asset error, crash), fail the smoke run.
                // std::process::exit so the failure code cannot be swallowed
                // by event-loop teardown (AppHandle::exit proved unreliable).
                std::thread::spawn(move || {
                    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
                    while std::time::Instant::now() < deadline {
                        std::thread::sleep(std::time::Duration::from_millis(250));
                    }
                    eprintln!("AI_EDITOR_SMOKE_TIMEOUT: webview never reported boot");
                    let _ = std::io::Write::flush(&mut std::io::stderr());
                    std::process::exit(1);
                });
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            pick_and_load_model,
            load_model_from_path,
            auto_load_model,
            console_history,
            loaded_model_path,
            hf_search,
            hf_download_model,
            hf_cancel_download,
            list_downloaded_models,
            api_server_start,
            api_server_stop,
            api_server_status,
            agent_attach_file,
            agent_detach_file,
            agent_list_attachments,
            voice_transcribe_data,
            updater_configured,
            update_check,
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
            mcp_catalog_load,
            mcp_catalog_save,
            knowledge_scan,
            knowledge_report_cmd,
            skill_set_active,
            skill_install,
            skill_uninstall,
            agent_respond_permission,
            agent_audit_log,
            agent_policy_snapshot,
            agent_set_yolo,
            agent_grant_path,
            agent_revoke_path,
            agent_git_checkpoint_cmd,
            agent_git_checkpoints_cmd,
            agent_git_revert_cmd,
            settings_load,
            settings_save,
            session_append,
            session_load,
            session_projects,
            session_delete_chat,
            smoke_active,
            smoke_boot_ok,
            smoke_fail,
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

#[cfg(test)]
mod tests {
    use super::{chat_key, session_file, session_key};

    #[test]
    fn session_key_matches_legacy_sanitization() {
        assert_eq!(session_key("D:\\ai"), "D__ai");
        assert_eq!(session_key("/home/user/proj"), "home_user_proj");
        assert_eq!(session_key("default"), "default");
        // Stable across calls (idempotent on its own output).
        assert_eq!(
            session_key(&session_key("C:\\x\\y")),
            session_key("C:\\x\\y")
        );
    }

    #[test]
    fn chat_key_is_filename_safe() {
        assert_eq!(chat_key("chat-ab12"), "chat-ab12");
        assert_eq!(chat_key("a/b\\c:d"), "a_b_c_d");
        assert_eq!(chat_key("../../../etc"), "etc");
        // No traversal can survive sanitization.
        let k = chat_key("..\\..\\evil");
        assert!(!k.contains('\\') && !k.contains('/'));
        assert_eq!(chat_key("   "), "chat");
    }

    #[test]
    fn default_chat_keeps_legacy_flat_file() {
        let dir = std::path::Path::new("S");
        assert_eq!(session_file(dir, "D:\\ai", None), dir.join("D__ai.jsonl"));
        assert_eq!(
            session_file(dir, "D:\\ai", Some("")),
            dir.join("D__ai.jsonl")
        );
        assert_eq!(
            session_file(dir, "D:\\ai", Some("chat-1")),
            dir.join("D__ai").join("chat-1.jsonl")
        );
    }
}
