// Prevents an extra console window on Windows in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod agent;
mod api_server;
mod commands;
mod engine;
mod hub;
mod logging;
mod remote;
mod terminal;
mod watcher;

use std::collections::HashMap;
use std::path::PathBuf;

use agent::context::{ContextManager, DEFAULT_LIMIT};
use agent::interrupt::InterruptState;
use agent::skills::{KnowledgeReport, KnowledgeState};
use agent::ToolState;
use crossbeam_channel::{bounded, Sender};
use engine::{
    EnginePool, InferenceDone, LoadProgressEvent, LocalGenerator, ModelInitParams, ModelInfo,
    TextGenerator, WorkerEvent,
};
use remote::ProviderRegistry;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

/// Shared inference state. The engine lives as an [`EnginePool`] — one worker
/// thread per generator, each owning its context/client for its whole life.
/// Model load (blocking) runs in `spawn_blocking`; generations dispatch to
/// workers over channels, so there is no `'static` transmute of the engine and
/// several generations can run concurrently (parallel sub-tasks).
pub(crate) struct InferenceState {
    pub(crate) pool: Mutex<Option<Arc<EnginePool>>>,
    pub(crate) info: Mutex<Option<ModelInfo>>,
    /// Absolute path of the GGUF currently loaded (shown next to the
    /// Load/Unload button); `None` when nothing is loaded.
    pub(crate) loaded_path: Mutex<Option<String>>,
    pub(crate) worker_tx: Mutex<Option<crossbeam_channel::Sender<WorkerEvent>>>,
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

/// Managed handle to the multi-provider [registry](ProviderRegistry). Backed
/// by a `tokio::Mutex` so both the frontend commands and the remote-load path
/// can share one registry across tasks. Starts empty, so every existing
/// single-provider code path is unaffected until a provider is registered.
pub(crate) struct ProviderRegistryState(pub(crate) tokio::sync::Mutex<ProviderRegistry>);

impl Default for ProviderRegistryState {
    fn default() -> Self {
        Self(tokio::sync::Mutex::new(ProviderRegistry::new()))
    }
}

impl InferenceState {
    /// Synchronous unload — can be called from a plain worker thread.
    /// Blocks until the pool is dropped.
    #[allow(dead_code)]
    pub fn unload_sync(&self) {
        *self.pool.blocking_lock() = None;
        *self.info.blocking_lock() = None;
        *self.loaded_path.blocking_lock() = None;
    }

    /// Synchronous local GGUF load — blocks until the model is loaded.
    #[allow(dead_code)]
    pub fn load_local_sync(
        &self,
        path: &str,
        app: &AppHandle,
    ) -> Result<ModelInfo, String> {
        let p = PathBuf::from(path);
        if !p.is_file() {
            return Err(format!("Model file not found: {path}"));
        }
        let params = ModelInitParams::default();
        let event_tx = self
            .worker_tx
            .blocking_lock()
            .clone()
            .unwrap_or_else(|| spawn_emitter(app.clone()));
        let app_clone = app.clone();
        let pool = build_local_pool(&p, &params, event_tx, &app_clone)?;
        let info = pool.info();
        *self.pool.blocking_lock() = Some(Arc::new(pool));
        *self.info.blocking_lock() = Some(info.clone());
        *self.loaded_path.blocking_lock() = Some(path.to_string());
        Ok(info)
    }
}

/// The token-budget engine managed as Tauri state.
pub struct ContextState {
    pub(crate) inner: Mutex<ContextManager>,
}

impl Default for ContextState {
    fn default() -> Self {
        Self {
            inner: Mutex::new(ContextManager::new(DEFAULT_LIMIT)),
        }
    }
}

/// Per-download control handle: cancellation + cooperative pause flag that
/// the streaming loop polls between chunks. Pausing keeps the `*.part` file so
/// a later `hf_resume_download` continues from the same byte offset.
#[derive(Clone)]
pub(crate) struct DownloadControl {
    pub(crate) cancel: CancellationToken,
    pub(crate) paused: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Wake token so the download loop can block for resume instead of spinning.
    pub(crate) resume: std::sync::Arc<tokio::sync::Notify>,
}

impl DownloadControl {
    pub(crate) fn new() -> Self {
        Self {
            cancel: CancellationToken::new(),
            paused: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            resume: std::sync::Arc::new(tokio::sync::Notify::new()),
        }
    }
}

/// Hugging Face download registry: one control per active `{repo}::{file}`
/// download so the UI can cancel, pause or resume it.
pub(crate) struct HubState {
    pub(crate) active: Arc<Mutex<HashMap<String, DownloadControl>>>,
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
pub(crate) struct ApiServerState {
    pub(crate) engine: api_server::SharedEngine,
    pub(crate) server: Mutex<Option<api_server::ApiServerHandle>>,
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
pub(crate) fn spawn_emitter(app: AppHandle) -> crossbeam_channel::Sender<WorkerEvent> {
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
pub(crate) fn build_local_pool(
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
pub(crate) async fn install_local_model(
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
    // Peek for a sibling `tokenizer.json` before `path` is moved into the
    // load task so we can register it below for exact-count compaction.
    let tokenizer_candidate = path
        .parent()
        .map(|d| d.join("tokenizer.json"))
        .filter(|p| p.is_file());
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

    // If a `tokenizer.json` ships next to the model, register it so the
    // orchestrator compacts its working history with exact token counts (it
    // falls back to the chars/4 heuristic when absent).
    if let Some(tok_path) = tokenizer_candidate {
        match context_state.inner.lock().await.load_tokenizer(&tok_path) {
            Ok(()) => logging::info(
                None,
                "context.tokenizer",
                &format!("registered `{}`", tok_path.display()),
            ),
            Err(e) => logging::info(
                None,
                "context.tokenizer",
                &format!("tokenizer load failed: {e}"),
            ),
        }
    }

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
    crate::commands::settings::persist_model_path(&app, &path_for_event).await;
    Ok(info)
}

// ---------------------------------------------------------------------------
// Paths + knowledge/rule plumbing
// ---------------------------------------------------------------------------

pub(crate) fn app_config_dir(app: &AppHandle) -> PathBuf {
    app.path()
        .app_config_dir()
        .unwrap_or_else(|_| std::env::temp_dir().join("ai-editor"))
}

pub(crate) fn app_data_dir(app: &AppHandle) -> PathBuf {
    app.path()
        .app_data_dir()
        .unwrap_or_else(|_| std::env::temp_dir().join("ai-editor-data"))
}

/// Push rules + active skills into the context manager as pinned buffers.
pub(crate) async fn sync_knowledge(context_state: &ContextState, knowledge: &KnowledgeState) {
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
    let memory = knowledge.memory_content(&knowledge.workspace());
    if memory.trim().is_empty() {
        inner.remove_pinned("memory");
    } else {
        let labeled = format!("### Memory (auto-extracted)\n{}\n\n", memory.trim());
        inner.upsert_pinned("memory", labeled);
    }
}

/// Load custom agent modes (`.ai/modes/*.md`) from `workspace` into the global
/// thread-local registry so tool-call enforcement sees them. Registered data is
/// `'static` (leaked) on purpose — see [`crate::agent::subagent::register_modes`]
/// and [`crate::agent::subagent::build_mode_profile`]. Returns the loaded modes.
pub(crate) fn sync_modes(workspace: &std::path::Path) -> Vec<crate::agent::subagent::Mode> {
    let modes = crate::agent::subagent::load_modes(workspace);
    crate::agent::subagent::register_modes(&modes);
    if !modes.is_empty() {
        logging::info(
            None,
            "agent.modes",
            &format!("{} custom mode(s) registered from {}", modes.len(), workspace.display()),
        );
    }
    modes
}

pub(crate) fn knowledge_report(knowledge: &KnowledgeState) -> KnowledgeReport {
    knowledge.report()
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Headless boot-smoke mode (CI / BN-12): with `AI_EDITOR_SMOKE=1` the app
/// boots for real (plugins, state, window, webview) and the frontend reports
/// a successful mount via `smoke_boot_ok`, which prints a marker and exits 0.
/// A watchdog thread fails the run if the webview never reports.
pub(crate) fn smoke_enabled() -> bool {
    std::env::var("AI_EDITOR_SMOKE")
        .map(|v| !v.trim().is_empty() && v.trim() != "0")
        .unwrap_or(false)
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
        .plugin(tauri_plugin_opener::init())
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
        .manage(ProviderRegistryState::default())
        .manage(ApiServerState::default())
        .manage(std::sync::Arc::new(ToolState {
            knowledge: knowledge.clone(),
            ..ToolState::default()
        }))
        .manage(InterruptState::default())
        .manage(ContextState::default())
        .manage(watcher::WatcherState::new())
        .manage(knowledge)
        .manage(std::sync::Arc::new(terminal::TerminalSessions::new()))
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
            commands::model::pick_and_load_model,
            commands::model::load_model_from_path,
            commands::model::auto_load_model,
            commands::misc::console_history,
            commands::model::loaded_model_path,
            commands::model::hf_search,
            commands::model::hf_download_model,
            commands::model::hf_pause_download,
            commands::model::hf_resume_download,
            commands::model::hf_list_active,
            commands::model::hf_cancel_download,
            commands::model::list_downloaded_models,
            commands::misc::system_info,
            commands::model::api_server_start,
            commands::model::api_server_stop,
            commands::model::api_server_status,
            commands::agent::agent_attach_file,
            commands::agent::agent_detach_file,
            commands::agent::agent_list_attachments,
            commands::agent::voice_transcribe_data,
            commands::misc::updater_configured,
            commands::misc::update_check,
            commands::model::configure_remote_model,
            commands::model::list_remote_models,
            commands::model::providers_upsert,
            commands::model::providers_remove,
            commands::model::providers_set_role,
            commands::model::providers_clear_role,
            commands::model::providers_route,
            commands::model::providers_list,
            commands::model::unload_model,
            commands::model::model_status,
            commands::agent::stream_inference,
            commands::agent::autocomplete_generate,
            commands::agent::cancel_inference,
            commands::agent::abort_agent_execution,
            commands::editor::terminal_spawn,
            commands::editor::terminal_write,
            commands::editor::terminal_kill,
            commands::editor::terminal_list,
            commands::agent::agent_run_task,
            commands::agent::agent_run_background,
            commands::agent::list_background_tasks,
            commands::agent::abort_background_task,
            commands::editor::pick_workspace_folder,
            commands::editor::pick_text_file,
            commands::editor::list_directory,
            commands::editor::read_text_file,
            commands::editor::write_text_file,
            commands::editor::revert_file,
            commands::editor::save_file_as,
            commands::editor::save_file_as_bytes,
            commands::editor::agent_set_workspace,
            commands::editor::agent_get_workspace,
            commands::editor::agent_get_workspaces,
            commands::editor::agent_add_workspace,
            commands::editor::agent_remove_workspace,
            commands::agent::agent_execute_tool,
            commands::agent::agent_batch_execute,
            commands::agent::agent_tool_schemas,
            commands::agent::agent_reset_mcp,
            commands::misc::mcp_catalog_load,
            commands::misc::mcp_catalog_save,
            commands::agent::knowledge_scan,
            commands::agent::knowledge_report_cmd,
            commands::agent::skill_set_active,
            commands::agent::skill_install,
            commands::agent::skill_import_dialog,
            commands::agent::skill_install_sample,
            commands::agent::skill_uninstall,
            commands::agent::agent_modes,
            commands::agent::agent_workflows,
            commands::agent::workflow_enforce_tools,
            commands::agent::agent_respond_permission,
            commands::agent::agent_respond_question,
            commands::agent::agent_audit_log,
            commands::agent::agent_policy_snapshot,
            commands::agent::agent_set_yolo,
            commands::agent::agent_grant_path,
            commands::agent::agent_revoke_path,
            commands::agent::agent_git_checkpoint_cmd,
            commands::agent::agent_git_checkpoints_cmd,
            commands::agent::agent_git_revert_cmd,
            commands::agent::agent_checkpoint_names_cmd,
            commands::settings::settings_load,
            commands::settings::settings_save,
            commands::session::session_append,
            commands::session::session_load,
            commands::session::session_projects,
            commands::session::session_delete_chat,
            commands::session::session_fork,
            commands::session::session_tail,
            commands::misc::smoke_active,
            commands::misc::smoke_boot_ok,
            commands::misc::smoke_fail,
            commands::context::context_status,
            commands::context::context_set_system_prompt,
            commands::context::context_set_file_buffer,
            commands::context::context_push_turn,
            watcher::start_file_watcher,
            watcher::stop_file_watcher,
            watcher::file_watcher_active,
        ])
        .run(tauri::generate_context!())
        .expect("error while running AI Editor");
}

fn main() {
    run();
}