//! Agent runtime IPC: task/stream/background execution, tool dispatch, RAG
//! attachments, knowledge & skills, permissions/audit/policy and git
//! checkpoints (moved out of `main.rs` during the command-layer split).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::Value;
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_dialog::DialogExt;
use tokio_util::sync::CancellationToken;

use crate::agent::background::start_background_task;
use crate::agent::core::tool_schemas;
use crate::agent::edits::{
    apply_edits, list_checkpoints, list_pending_edits, reject_edits, resolve_conflict_edits,
    restore_checkpoints,
};
use crate::agent::interrupt::{AbortPayload, InterruptState};
use crate::agent::orchestrator::{run_agent_loop_pool, AgentTaskRequest};
use crate::agent::policy::{save_policy_with_matrix, snapshot, PolicyRule, PolicySnapshot};
use crate::agent::skills::{KnowledgeReport, KnowledgeState};
use crate::agent::subagent::{builtin_modes, register_workflow_tools, Mode};
use crate::agent::tools::{dispatch, git_checkpoint, git_checkpoints, git_revert, transcribe_file};
use crate::agent::workflows::{load_workflows, Workflow};
use crate::agent::{
    now_ms, BackgroundTaskEvent, BackgroundTaskInfo, EventSink, FileCheckpoint, FileEdit,
    PathGrant, PendingEditGroup, PermissionDecision, ToolCall, ToolContext, ToolResult, ToolState,
};
use crate::engine::{
    ChatTurn, EnginePool, InferenceRequest, RuntimeRouter, TextGenerator, WorkerEvent,
};
use crate::remote::{ProviderKind, ProviderRole, RemoteGenerator};
use crate::{
    app_config_dir, app_data_dir, knowledge_report, spawn_emitter, sync_knowledge, sync_modes,
    ContextState, InferenceState, ProviderRegistryState,
};

// ---------------------------------------------------------------------------
// EventSink adapter — bridges agent/ events to the Tauri frontend
// ---------------------------------------------------------------------------

/// Wraps a Tauri [`AppHandle`] behind [`agent::EventSink`] so the agent
/// core stays free of `tauri`/`Emitter` imports.
struct TauriEventSink(AppHandle);

impl EventSink for TauriEventSink {
    fn emit_value(&self, event: &str, payload: &serde_json::Value) {
        let _ = self.0.emit(event, payload);
    }
}

/// Build a [`ToolContext`] from the current Tauri [`AppHandle`]. Called once
/// per command invocation; the `Arc` and `PathBuf` are cheap to clone.
fn tool_ctx(app: &AppHandle) -> ToolContext {
    ToolContext {
        sink: Arc::new(TauriEventSink(app.clone())),
        config_dir: app
            .path()
            .app_config_dir()
            .unwrap_or_else(|_| std::env::temp_dir().join("ai-editor")),
    }
}

/// Resolve the model pool a task should run against. Returns the loaded pool
/// when one exists; otherwise — if the Settings → Providers registry routes
/// the Editor role to a remote provider — builds and memorises a
/// `RemoteGenerator` pool from that config. That lets chat reach an external
/// provider immediately after it is registered, without a separate load step.
async fn active_pool(
    app: &AppHandle,
    state: &State<'_, InferenceState>,
    providers: &State<'_, ProviderRegistryState>,
) -> Result<Arc<EnginePool>, String> {
    {
        let held = state.pool.lock().await.clone();
        if let Some(p) = held {
            return Ok(p);
        }
    }
    let reg = providers.0.lock().await.clone();
    let Some(provider) = reg.route(ProviderRole::Editor) else {
        return Err("No model loaded — load a model or register an Editor provider first.".into());
    };
    if provider.kind == ProviderKind::Local {
        return Err("No model loaded — load a local GGUF from the Models page first.".into());
    }
    let config = provider
        .to_remote_model_config()
        .map_err(|e| format!("Provider '{}' cannot be used: {}", provider.id, e))?;
    let event_tx = state
        .worker_tx
        .lock()
        .await
        .clone()
        .unwrap_or_else(|| spawn_emitter(app.clone()));
    let factory =
        || RemoteGenerator::new(config.clone()).map(|g| Box::new(g) as Box<dyn TextGenerator>);
    let pool = EnginePool::spawn_with(factory, event_tx, 4usize)
        .map_err(|e| format!("Failed to start remote provider '{}': {e}", provider.id))?;
    let info = pool.info();
    *state.pool.lock().await = Some(Arc::new(pool));
    *state.info.lock().await = Some(info.clone());
    let activated = state
        .pool
        .lock()
        .await
        .clone()
        .ok_or_else(|| "remote pool vanished".to_string())?;
    let _ = app.emit("model-loaded", activated.info());
    crate::logging::info(
        None,
        "llm.request",
        &format!(
            "auto-activated remote provider `{}` (model {})",
            provider.id, provider.model
        ),
    );
    Ok(activated)
}

// ---------------------------------------------------------------------------
// RAG attachments (UI paperclip flow shares the agent's index)
// ---------------------------------------------------------------------------

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AttachedFileInfo {
    path: String,
    bytes: u64,
    // Serialized as `chunkCount` via rename_all above.
    chunk_count: usize,
}

/// Attach a file from the UI into the same session index used by the agent.
#[tauri::command]
pub async fn agent_attach_file(
    state: State<'_, Arc<ToolState>>,
    path: String,
) -> Result<AttachedFileInfo, String> {
    let text = tokio::fs::read_to_string(&path)
        .await
        .map_err(|e| format!("Failed to read: {e}"))?;
    let file = { state.rag.lock().unwrap().attach(&path, &text)? };
    Ok(AttachedFileInfo {
        path,
        bytes: file.bytes,
        chunk_count: file.chunk_count,
    })
}

/// Detach a file from the session index (UI chip ✕).
#[tauri::command]
pub async fn agent_detach_file(
    state: State<'_, Arc<ToolState>>,
    path: String,
) -> Result<(), String> {
    if !state.rag.lock().unwrap().detach(&path) {
        return Err(format!("`{path}` is not attached"));
    }
    Ok(())
}

/// List currently attached files.
#[tauri::command]
pub async fn agent_list_attachments(
    state: State<'_, Arc<ToolState>>,
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
pub async fn voice_transcribe_data(
    app: AppHandle,
    state: State<'_, Arc<ToolState>>,
    data: Vec<u8>,
    ext: Option<String>,
) -> Result<String, String> {
    let dir = app_data_dir(&app).join("voice");
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| format!("mkdir failed: {e}"))?;
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
    tokio::fs::write(&path, &data)
        .await
        .map_err(|e| format!("Write failed: {e}"))?;
    transcribe_file(&state, &path).await
}

// ---------------------------------------------------------------------------
// Streaming + agent task execution
// ---------------------------------------------------------------------------

#[derive(serde::Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct StartedEvent {
    session_id: u64,
}

/// Streaming inference. Dispatches to a worker in the pool (round-robin); the
/// worker's own thread holds the generator for the whole generation. A fresh
/// circuit-breaker token is armed per run so an old cancellation can never leak
/// into this one.
///
/// Also serves as the Ask-mode chat-complete path (single turn, zero tool
/// loop): the frontend calls it directly when "Ask uses chat completion" is
/// enabled. Resolves the engine through [`active_pool`], so a registered
/// remote Editor provider works without a local model.
///
/// The conversation snapshot from the [`crate::ContextState`] manager (which the
/// frontend populates before invoking — the user turn is already the last
/// entry) is attached as structured messages so the engine can render it
/// through the model's own chat template; the flat `request.prompt` remains
/// the fallback for template-less models.
#[tauri::command]
pub async fn stream_inference(
    app: AppHandle,
    state: State<'_, InferenceState>,
    providers: State<'_, ProviderRegistryState>,
    interrupt_state: State<'_, InterruptState>,
    context_state: State<'_, ContextState>,
    request: InferenceRequest,
) -> Result<u64, String> {
    let pool = active_pool(&app, &state, &providers).await?;

    // Attach the conversation (system prompt + pinned buffers + history).
    let mut messages = {
        let snapshot = context_state.inner.lock().await.messages();
        snapshot
            .into_iter()
            .map(|m| ChatTurn {
                role: m.role,
                content: m.content,
            })
            .collect::<Vec<_>>()
    };
    // The frontend pushes the user turn before invoking; only append when the
    // snapshot does not already end with this exact turn (dedup mirrors
    // orchestrator's build_prompt behaviour).
    let tail_dupes = matches!(
        messages.last(),
        Some(m) if m.role == "user" && m.content.trim() == request.prompt.trim()
    );
    if !tail_dupes {
        messages.push(ChatTurn {
            role: "user".into(),
            content: request.prompt.clone(),
        });
    }
    let mut request = request;
    request.messages = Some(messages);

    let session_id = interrupt_state.next_session();
    let interrupt = interrupt_state.arm();
    let _ = app.emit("inference-started", StartedEvent { session_id });
    crate::logging::info(
        Some(session_id),
        "llm.request",
        &format!(
            "chat request · {} chars · max_tokens={} · {}",
            request.prompt.chars().count(),
            request.max_tokens,
            if crate::logging::prompt_preview_enabled() {
                format!("prompt: {}", crate::logging::preview(&request.prompt, 80))
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
                crate::logging::warn(
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
pub async fn agent_run_task(
    app: AppHandle,
    state: State<'_, InferenceState>,
    providers: State<'_, ProviderRegistryState>,
    interrupt_state: State<'_, InterruptState>,
    context_state: State<'_, ContextState>,
    tool_state: State<'_, Arc<ToolState>>,
    request: AgentTaskRequest,
) -> Result<u64, String> {
    run_agent_task_inner(
        app,
        &state,
        &providers,
        &interrupt_state,
        &context_state,
        &tool_state,
        request,
    )
    .await
}

/// Shared core of [`agent_run_task`], also driving `POST /v1/agent/tasks` on the
/// loopback HTTP server (P3.2). The HTTP route builds the same managed `State`
/// handles from its `AppHandle` via `Manager::state`.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_agent_task_inner(
    app: AppHandle,
    state: &State<'_, InferenceState>,
    providers: &State<'_, ProviderRegistryState>,
    interrupt_state: &State<'_, InterruptState>,
    context_state: &State<'_, ContextState>,
    tool_state: &State<'_, Arc<ToolState>>,
    request: AgentTaskRequest,
) -> Result<u64, String> {
    let pool = active_pool(&app, state, providers).await?;
    {
        let mut ctx = context_state.inner.lock().await;
        let snapshot = ctx.messages();
        let system_text = snapshot
            .iter()
            .find(|m| m.role == "system")
            .map(|m| m.content.as_str())
            .unwrap_or("");
        if !crate::agent::prompt::ensure_system_prompt(system_text) {
            ctx.set_system_prompt(crate::agent::prompt::DEFAULT_AGENT_SYSTEM.to_string());
        }
    }
    let session_id = interrupt_state.next_session();
    // P3.1 session_start hooks: run once per task before the loop starts. Any
    // stdout is pinned into the context as a "hooks" buffer so the model sees
    // it (build_prompt renders it under "## Session hook output").
    {
        let workspaces = tool_state.workspace.lock().await.clone();
        if let Some(ws) = workspaces.first() {
            let cfg = crate::agent::hooks::load(ws);
            if !cfg.session_start.is_empty() {
                let payload = crate::agent::hooks::payload("session_start", ws, None, session_id);
                if let Some(out) =
                    crate::agent::hooks::run_session_start(ws, &cfg.session_start, &payload).await
                {
                    context_state
                        .inner
                        .lock()
                        .await
                        .upsert_pinned("hooks", out.clone());
                    let _ = app.emit(
                        "agent://hook",
                        serde_json::json!({
                            "event": "session_start",
                            "status": "ok",
                            "detail": out,
                            "sessionId": session_id,
                        }),
                    );
                }
            }
        }
    }
    // Snapshot the role→provider registry so the worker thread can route
    // plan/memory phases to a different provider at runtime (P0 hand-off).
    let role_registry = providers.0.lock().await.clone();

    let interrupt = interrupt_state.arm();
    let _ = app.emit("inference-started", StartedEvent { session_id });
    crate::logging::info(
        Some(session_id),
        "llm.request",
        &format!(
            "agent task · {} chars · max_steps={} plan_mode={} decompose={} verify={}{}",
            request.prompt.chars().count(),
            request.max_steps.unwrap_or(6),
            request.plan_mode,
            request.decompose,
            request.verify,
            if crate::logging::prompt_preview_enabled() {
                format!(
                    " · prompt: {}",
                    crate::logging::preview(&request.prompt, 80)
                )
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

    let tool_state_arc = Arc::clone(tool_state);

    let context_snapshot = context_state.inner.lock().await.messages();
    // Share the ContextManager's registered HF tokenizer (if any) so the
    // orchestrator compacts its working history with exact token counts.
    let context_tokenizer = context_state.inner.lock().await.tokenizer();
    let ctx = tool_ctx(&app);
    let tx_clone = tx.clone();
    let context_budget = pool.info().context_size as usize;
    let pool_for_thread = pool.clone();

    std::thread::spawn(move || {
        let result = run_agent_loop_pool(
            &pool_for_thread,
            &tool_state_arc,
            &ctx,
            &interrupt,
            &tx_clone,
            session_id,
            &context_snapshot,
            &request,
            context_budget,
            Some(role_registry),
            context_tokenizer,
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

// ---------------------------------------------------------------------------
// Background tasks (P2-12)
// ---------------------------------------------------------------------------

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AutocompleteRequest {
    prefix: String,
    suffix: Option<String>,
    max_tokens: Option<u32>,
}

/// Monaco inline AI completion: given the code before the cursor (`prefix`)
/// and optionally the text after it (`suffix`), generate a short fill-in
/// snippet. Routed through the `Autocomplete` provider role when one is
/// configured (falls back to the local pool). Runs non-streaming with
/// session id 0 so it never renders into a chat stream.
#[tauri::command]
pub async fn autocomplete_generate(
    state: State<'_, InferenceState>,
    providers: State<'_, ProviderRegistryState>,
    request: AutocompleteRequest,
) -> Result<String, String> {
    let pool = state.pool.lock().await.clone().ok_or("No model loaded")?;
    let role_registry = providers.0.lock().await.clone();
    // Throwaway sender: completions are invisible to the UI event bus.
    let (tx, _) = crossbeam_channel::unbounded();

    // Fill-in-the-middle style instruction tuned for code completion. For
    // local models we keep it minimal; the model just continues the code.
    let mut prompt = String::new();
    if let Some(suffix) = request.suffix.as_deref().filter(|s| !s.trim().is_empty()) {
        prompt.push_str(&format!(
            "Complete the following code. Insert exactly the missing middle (return only the inserted text, no explanation, no wrapping backticks).\n\n<fim_prefix>{prefix}<fim_suffix>{suffix}<fim_middle>",
            prefix = request.prefix,
            suffix = suffix
        ));
    } else {
        prompt.push_str(&format!(
            "Complete the following code. Return only the continuation (the missing text right after the cursor), no explanation and no enclosing code fences.\n\n{prefix}",
            prefix = request.prefix
        ));
    }

    let max_tokens = request.max_tokens.unwrap_or(128).clamp(16, 512);
    let gen_request = InferenceRequest {
        prompt,
        messages: None,
        images: None,
        max_tokens,
        stop_words: Some(vec![
            "<|endoftext|>".into(),
            "<|fim_middle|>".into(),
            "\n\n".into(),
        ]),
        cached_prefix_tokens: None,
        ..Default::default()
    };

    let interrupt = CancellationToken::new();
    let mut router = RuntimeRouter::new(&pool, role_registry, tx.clone());
    let mut gen = router.resolve(ProviderRole::Autocomplete)?;
    // Session id 0 keeps the completion off the chat stream. `generate` is
    // CPU-bound (llama.cpp) so run it on the blocking pool instead of stalling
    // the async runtime that drives the rest of the app.
    let outcome =
        tokio::task::spawn_blocking(move || gen.generate(&gen_request, 0, &interrupt, &tx))
            .await
            .map_err(|e| format!("autocomplete task failed: {e}"))??;
    Ok(outcome.full_text.trim().to_string())
}

/// Start an agent task that runs in the background, independent of the
/// foreground chat. The task gets its own cancellation token and is tracked
/// in `ToolState.background_tasks` until completion or abort.
#[tauri::command]
pub async fn agent_run_background(
    app: AppHandle,
    state: State<'_, InferenceState>,
    providers: State<'_, ProviderRegistryState>,
    interrupt_state: State<'_, InterruptState>,
    context_state: State<'_, ContextState>,
    tool_state: State<'_, Arc<ToolState>>,
    request: AgentTaskRequest,
) -> Result<u64, String> {
    let pool = active_pool(&app, &state, &providers).await?;
    let role_registry = providers.0.lock().await.clone();
    let tx = state
        .worker_tx
        .lock()
        .await
        .clone()
        .unwrap_or_else(|| spawn_emitter(app.clone()));
    let context_snapshot = context_state.inner.lock().await.messages();
    let context_budget = pool.info().context_size as usize;
    // Share the registered HF tokenizer so the orchestrator compacts history
    // with exact token counts (falls back to the chars/4 heuristic when absent).
    let context_tokenizer = context_state.inner.lock().await.tokenizer();

    let ctx = tool_ctx(&app);
    start_background_task(
        pool,
        &tool_state,
        &ctx,
        &interrupt_state,
        &tx,
        &request,
        &context_snapshot,
        context_budget,
        Some(role_registry),
        context_tokenizer,
    )
}

/// List all currently running background tasks.
#[tauri::command]
pub async fn list_background_tasks(
    tool_state: State<'_, Arc<ToolState>>,
) -> Result<Vec<BackgroundTaskInfo>, String> {
    Ok(tool_state.background_tasks.list())
}

/// Abort a specific background task by its id.
#[tauri::command]
pub async fn abort_background_task(
    app: AppHandle,
    tool_state: State<'_, Arc<ToolState>>,
    task_id: String,
) -> Result<(), String> {
    let info = tool_state
        .background_tasks
        .abort(&task_id)
        .ok_or_else(|| format!("Unknown background task: {task_id}"))?;
    tool_state
        .background_output
        .record(info.session_id, "aborted");
    tool_state.background_output.flush(info.session_id);

    // Emit lifecycle event so the frontend cleans up.
    let _ = app.emit(
        "agent://bg-task-event",
        BackgroundTaskEvent {
            task_id: info.id,
            session_id: info.session_id,
            label: info.label,
            status: "aborted".into(),
            detail: None,
        },
    );

    Ok(())
}

/// Tail the captured output ring buffer of a background task. Works for
/// finished tasks too — the buffer outlives the registry entry.
#[tauri::command]
pub async fn background_task_output(
    tool_state: State<'_, Arc<ToolState>>,
    task_id: String,
    tail_lines: Option<usize>,
) -> Result<Vec<String>, String> {
    Ok(tool_state.background_output.tail(&task_id, tail_lines))
}

/// Acknowledgement for a queued steer: the id that will come back on the
/// `agent://steer-applied` event, and the session it was queued against.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SteerAck {
    pub id: String,
    pub session_id: u64,
}

/// Cancel the in-flight generation (if any) via the circuit breaker.
#[tauri::command]
pub async fn cancel_inference(state: State<'_, InterruptState>) -> Result<(), String> {
    state.trigger();
    Ok(())
}

/// Emergency abort for any running job (LLM generation, terminal sub-process,
/// MCP call). Cancels the circuit breaker instantly and returns an
/// "Execution Aborted" payload to the UI.
#[tauri::command]
pub async fn abort_agent_execution(
    app: AppHandle,
    state: State<'_, InterruptState>,
) -> Result<AbortPayload, String> {
    state.trigger();
    let payload = state.payload(state.current_session());
    let _ = app.emit("execution-aborted", &payload);
    Ok(payload)
}

/// Queue a message to be injected into the run that is already in flight.
///
/// Unlike `abort_agent_execution` this does not cancel anything: the message is
/// folded into the conversation at the top of the next agent step, so the
/// current generation finishes and the agent then reacts to the new
/// instruction. The returned id is echoed back on the `agent://steer-applied`
/// event, letting the UI move the message out of its queue.
///
/// Rejected when no run is active, because a message queued against a finished
/// run would silently never be applied and the user would think it was sent.
#[tauri::command]
pub async fn agent_steer(
    tool_state: State<'_, Arc<ToolState>>,
    interrupt_state: State<'_, InterruptState>,
    text: String,
) -> Result<SteerAck, String> {
    let text = text.trim().to_string();
    if text.is_empty() {
        return Err("Nothing to steer with — the message is empty.".into());
    }
    let session_id = interrupt_state.current_session();
    if session_id == 0 || interrupt_state.current().is_cancelled() {
        return Err("No agent run is in progress to steer.".into());
    }
    let id = tool_state.enqueue_steer(session_id, text);
    Ok(SteerAck { id, session_id })
}

// ---------------------------------------------------------------------------
// Direct tool dispatch (bypasses the orchestrator loop)
// ---------------------------------------------------------------------------

/// Execute a single tool call. Real-time progress is pushed over the
/// `agent://tool-event` event channel; the returned `ToolResult` is the
/// authoritative outcome. The circuit breaker token is handed to the tool so
/// sub-processes can be aborted mid-flight.
#[tauri::command]
pub async fn agent_execute_tool(
    app: AppHandle,
    state: State<'_, Arc<ToolState>>,
    interrupt_state: State<'_, InterruptState>,
    call: ToolCall,
) -> Result<ToolResult, String> {
    let ctx = tool_ctx(&app);
    dispatch(&ctx, &state, &call, interrupt_state.current()).await
}

/// Execute a batch of tool calls sequentially, returning results in order.
/// The orchestrator can fire multiple independent reads/writes in one trip.
#[tauri::command]
pub async fn agent_batch_execute(
    app: AppHandle,
    state: State<'_, Arc<ToolState>>,
    interrupt_state: State<'_, InterruptState>,
    calls: Vec<ToolCall>,
) -> Result<Vec<ToolResult>, String> {
    let ctx = tool_ctx(&app);
    let mut results = Vec::with_capacity(calls.len());
    for call in calls {
        results.push(dispatch(&ctx, &state, &call, interrupt_state.current()).await?);
    }
    Ok(results)
}

/// JSON schemas describing every tool, for orchestrator-side validation.
#[tauri::command]
pub async fn agent_tool_schemas() -> Result<HashMap<String, serde_json::Value>, String> {
    let mut schemas = HashMap::new();
    for (name, schema) in tool_schemas() {
        schemas.insert(name.to_string(), schema);
    }
    Ok(schemas)
}

/// Drop all cached MCP server connections (they respawn on demand).
#[tauri::command]
pub async fn agent_reset_mcp(state: State<'_, Arc<ToolState>>) -> Result<(), String> {
    state.mcp_servers.lock().await.clear();
    Ok(())
}

// ---------------------------------------------------------------------------
// Custom modes, workflows, skills & rules
// ---------------------------------------------------------------------------

/// List the custom agent modes available in the active workspace
/// (`.ai/modes/*.md`). The registry is refreshed on every call so editing a
/// mode file takes effect without restarting the app.
#[tauri::command]
pub async fn agent_modes(state: State<'_, Arc<ToolState>>) -> Result<Vec<Mode>, String> {
    let workspace = state.primary_workspace().await.unwrap_or_default();
    let mut modes = builtin_modes();
    modes.extend(sync_modes(&workspace));
    Ok(modes)
}

/// List the user-defined workflows available in the active workspace
/// (`.ai/workflows/*.md`) as a `/command` picker catalog.
#[tauri::command]
pub async fn agent_workflows(state: State<'_, Arc<ToolState>>) -> Result<Vec<Workflow>, String> {
    let workspace = state.primary_workspace().await.unwrap_or_default();
    Ok(load_workflows(&workspace))
}

/// Activate a workflow's tool allow-list for the current run scope so
/// `allowedTools:` enforcement applies to the invoking session.
#[tauri::command]
pub async fn workflow_enforce_tools(
    name: String,
    allowed_tools: Vec<String>,
) -> Result<(), String> {
    register_workflow_tools(&name, allowed_tools);
    Ok(())
}

/// (Re)scan the workspace for `.ai/skills`, `.ai/rules`, AGENTS.md, etc.
#[tauri::command]
pub async fn knowledge_scan(
    app: AppHandle,
    state: State<'_, Arc<ToolState>>,
    context_state: State<'_, ContextState>,
    knowledge_state: State<'_, Arc<KnowledgeState>>,
) -> Result<KnowledgeReport, String> {
    let ws = state
        .primary_workspace()
        .await
        .ok_or("No workspace set - open a workspace first")?;
    let config_dir = app_config_dir(&app);
    knowledge_state.scan(&ws, &config_dir)?;
    sync_knowledge(&context_state, &knowledge_state).await;
    Ok(knowledge_report(&knowledge_state))
}

/// Current knowledge snapshot (rules + all skills with active flags).
#[tauri::command]
pub async fn knowledge_report_cmd(
    knowledge_state: State<'_, Arc<KnowledgeState>>,
) -> Result<KnowledgeReport, String> {
    Ok(knowledge_report(&knowledge_state))
}

/// Toggle a skill in/out of the active context.
#[tauri::command]
pub async fn skill_set_active(
    app: AppHandle,
    knowledge_state: State<'_, Arc<KnowledgeState>>,
    context_state: State<'_, ContextState>,
    name: String,
    active: bool,
) -> Result<KnowledgeReport, String> {
    knowledge_state.set_active(&name, active)?;
    sync_knowledge(&context_state, &knowledge_state).await;
    let _ = app.emit("agent-knowledge", knowledge_report(&knowledge_state));
    let _ = app.emit("agent://skills-changed", ());
    Ok(knowledge_report(&knowledge_state))
}

/// Install a skill from a `.md` file or a folder containing `SKILL.md`.
/// `global` targets the user-global skills dir; otherwise the workspace's
/// `.ai/skills` is used. Rescans and returns the fresh report.
#[tauri::command]
pub async fn skill_install(
    app: AppHandle,
    state: State<'_, Arc<ToolState>>,
    context_state: State<'_, ContextState>,
    knowledge_state: State<'_, Arc<KnowledgeState>>,
    source: String,
    global: bool,
) -> Result<KnowledgeReport, String> {
    let ws = state
        .primary_workspace()
        .await
        .ok_or("No workspace set - open a workspace first")?;
    let config_dir = app_config_dir(&app);
    KnowledgeState::install(&ws, &config_dir, Path::new(&source), global)?;
    knowledge_state.scan(&ws, &config_dir)?;
    sync_knowledge(&context_state, &knowledge_state).await;
    let _ = app.emit("agent-knowledge", knowledge_report(&knowledge_state));
    let _ = app.emit("agent://skills-changed", ());
    Ok(knowledge_report(&knowledge_state))
}

/// Open a native picker for a skill — a `.md` file (`kind = "file"`) or a
/// folder containing `SKILL.md` (`kind = "folder"`) — then install it into
/// the workspace skills dir (`global = false`) or the user-global skills dir
/// (`global = true`) and rescan. Returns `None` when the dialog is cancelled.
#[tauri::command]
pub async fn skill_import_dialog(
    app: AppHandle,
    state: State<'_, Arc<ToolState>>,
    context_state: State<'_, ContextState>,
    knowledge_state: State<'_, Arc<KnowledgeState>>,
    kind: String,
    global: bool,
) -> Result<Option<KnowledgeReport>, String> {
    let picked = if kind == "folder" {
        app.dialog()
            .file()
            .set_title("Select a skill folder (must contain SKILL.md)")
            .blocking_pick_folder()
    } else {
        app.dialog()
            .file()
            .set_title("Select a skill file (.md)")
            .add_filter("Skill markdown", &["md"])
            .blocking_pick_file()
    };
    let Some(entry) = picked.and_then(|p| p.into_path().ok()) else {
        return Ok(None);
    };
    let source = entry.to_string_lossy().into_owned();
    let ws = state
        .primary_workspace()
        .await
        .ok_or("No workspace set - open a project first")?;
    let config_dir = app_config_dir(&app);
    KnowledgeState::install(&ws, &config_dir, Path::new(&source), global)?;
    knowledge_state.scan(&ws, &config_dir)?;
    sync_knowledge(&context_state, &knowledge_state).await;
    let report = knowledge_report(&knowledge_state);
    let _ = app.emit("agent-knowledge", &report);
    let _ = app.emit("agent://skills-changed", ());
    Ok(Some(report))
}

/// Install a sample skill directly from a name + markdown body (Settings →
/// Skills → "Install sample skill"). Writes `<name>.md` into the workspace
/// skills dir (or the global dir when `global`), validates and rescans.
#[tauri::command]
pub async fn skill_install_sample(
    app: AppHandle,
    state: State<'_, Arc<ToolState>>,
    context_state: State<'_, ContextState>,
    knowledge_state: State<'_, Arc<KnowledgeState>>,
    name: String,
    content: String,
    global: bool,
) -> Result<KnowledgeReport, String> {
    let ws = state
        .primary_workspace()
        .await
        .ok_or("No workspace set - open a workspace first")?;
    let config_dir = app_config_dir(&app);
    knowledge_state.install_sample(&ws, &config_dir, &name, &content, global)?;
    knowledge_state.scan(&ws, &config_dir)?;
    sync_knowledge(&context_state, &knowledge_state).await;
    let report = knowledge_report(&knowledge_state);
    let _ = app.emit("agent-knowledge", &report);
    let _ = app.emit("agent://skills-changed", ());
    Ok(report)
}

/// Delete a skill (file or folder) from disk. Rescans and returns the report.
#[tauri::command]
pub async fn skill_uninstall(
    app: AppHandle,
    state: State<'_, Arc<ToolState>>,
    context_state: State<'_, ContextState>,
    knowledge_state: State<'_, Arc<KnowledgeState>>,
    name: String,
) -> Result<KnowledgeReport, String> {
    let ws = state
        .primary_workspace()
        .await
        .ok_or("No workspace set - open a workspace first")?;
    let config_dir = app_config_dir(&app);
    knowledge_state.uninstall(&name)?;
    knowledge_state.scan(&ws, &config_dir)?;
    sync_knowledge(&context_state, &knowledge_state).await;
    let _ = app.emit("agent-knowledge", knowledge_report(&knowledge_state));
    let _ = app.emit("agent://skills-changed", ());
    Ok(knowledge_report(&knowledge_state))
}

// ---------------------------------------------------------------------------
// Permissions
// ---------------------------------------------------------------------------

/// Complete a pending permission-approval request from the UI. The decision
/// selects the remembered scope: `allow_once`, `allow_session`,
/// `always_allow` or `deny` (anything else is treated as a deny).
#[tauri::command]
pub async fn agent_respond_permission(
    state: State<'_, Arc<ToolState>>,
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

/// Complete a pending `ask_question` request from the UI (P1-9). The answer
/// (a preset choice or free text) is delivered to the blocked agent loop.
#[tauri::command]
pub async fn agent_respond_question(
    state: State<'_, Arc<ToolState>>,
    request_id: String,
    answer: String,
) -> Result<(), String> {
    let mut reqs = state.pending_questions.lock().await;
    if let Some(tx) = reqs.remove(&request_id) {
        let _ = tx.send(answer);
    }
    Ok(())
}

/// Complete a pending `<hitl_gate>` approval request from the UI. The verdict
/// (`approved = true/false`) is delivered to the blocked agent loop, which
/// either dispatches the queued tool-call batch or bounces the model toward a
/// safer alternative.
#[tauri::command]
pub async fn agent_respond_hitl(
    state: State<'_, Arc<ToolState>>,
    request_id: String,
    approved: bool,
) -> Result<(), String> {
    let mut reqs = state.pending_hitl.lock().await;
    let tx = reqs
        .remove(&request_id)
        .ok_or_else(|| "No pending approval request with that id".to_string())?;
    let _ = tx.send(crate::agent::HitlDecision { approve: approved });
    Ok(())
}

/// One tool-decision record from `{workspace}/.ai/audit.jsonl` (camelCase,
/// matching the `AuditEntry` type the frontend renders).
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AuditEntry {
    ts: u64,
    id: String,
    tool: String,
    summary: String,
    decision: String,
    #[serde(default)]
    started_at: Option<u64>,
    #[serde(default)]
    latency_ms: u64,
    #[serde(default)]
    success: Option<bool>,
    #[serde(default)]
    error: Option<String>,
}

/// Recent audit-log entries (`{workspace}/.ai/audit.jsonl`), newest first.
#[tauri::command]
pub async fn agent_audit_log(
    state: State<'_, Arc<ToolState>>,
    limit: Option<usize>,
) -> Result<Vec<AuditEntry>, String> {
    let ws = state.primary_workspace().await;
    let Some(ws) = ws else { return Ok(Vec::new()) };
    let path = ws.join(".ai/audit.jsonl");
    let text = tokio::fs::read_to_string(&path)
        .await
        .map_err(|e| format!("Failed to read the audit log: {e}"))?;
    let mut entries: Vec<AuditEntry> = text
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
pub async fn agent_policy_snapshot(
    state: State<'_, Arc<ToolState>>,
) -> Result<PolicySnapshot, String> {
    let ws = state.primary_workspace().await;
    let mut snap = snapshot(ws.as_deref());
    snap.yolo = Some(state.yolo.load(std::sync::atomic::Ordering::SeqCst));
    snap.path_grants = state.path_grants.lock().unwrap().iter().cloned().collect();
    Ok(snap)
}

/// Toggle the YOLO sub-mode (Bionic §3.3): ROUTINE shell commands skip the
/// approval dialog; red-zone stays denied unconditionally. Session-only.
#[tauri::command]
pub async fn agent_set_yolo(state: State<'_, Arc<ToolState>>, on: bool) -> Result<(), String> {
    state.yolo.store(on, std::sync::atomic::Ordering::SeqCst);
    Ok(())
}

/// Persist the workspace policy from the policy editor. The injected
/// `__red_zone__` rule is dropped before writing (it is UI-read-only and is
/// re-injected on every load; `save_policy` would reject it outright).
#[tauri::command]
pub async fn agent_policy_save(
    state: State<'_, Arc<ToolState>>,
    default: String,
    rules: Vec<PolicyRule>,
    auto_approve: Option<std::collections::BTreeMap<String, bool>>,
) -> Result<(), String> {
    let ws = state.primary_workspace().await;
    let editable: Vec<PolicyRule> = rules
        .into_iter()
        .filter(|r| r.tool != "__red_zone__")
        .collect();
    save_policy_with_matrix(
        ws.as_deref(),
        &default,
        &editable,
        &auto_approve.unwrap_or_default(),
    )
}

/// Grant a per-session `{path, mode}` allowance for a path OUTSIDE the
/// workspace (Bionic §3.3). mode: "read" | "write". Session-only — never
/// persisted to disk.
#[tauri::command]
pub async fn agent_grant_path(
    state: State<'_, Arc<ToolState>>,
    path: String,
    mode: String,
) -> Result<(), String> {
    if mode != "read" && mode != "write" {
        return Err("mode must be \"read\" or \"write\".".into());
    }
    let p = PathBuf::from(path.trim());
    if !p.is_absolute() {
        return Err("path must be absolute.".into());
    }
    let mut grants = state.path_grants.lock().unwrap();
    grants.retain(|g| g.path != p);
    grants.push(PathGrant { path: p, mode });
    Ok(())
}

/// Revoke every grant covering `path` (exact match or descendant).
#[tauri::command]
pub async fn agent_revoke_path(
    state: State<'_, Arc<ToolState>>,
    path: String,
) -> Result<(), String> {
    let p = PathBuf::from(path.trim());
    let mut grants = state.path_grants.lock().unwrap();
    grants.retain(|g| g.path != p && !g.path.starts_with(&p));
    Ok(())
}

// ---------------------------------------------------------------------------
// Git checkpoints / named snapshots
// ---------------------------------------------------------------------------

/// A checkpoint commit shown by the revert UI (`agent_git_checkpoints_cmd`).
#[derive(serde::Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CheckpointInfo {
    hash: String,
    subject: String,
    relative: String,
    /// User-given name when this checkpoint was created with `name`,
    /// resolved from `.ai/checkpoints.json`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<String>,
}

/// A user-named snapshot (`.ai/checkpoints.json`), keyed by commit hash.
#[derive(serde::Serialize, serde::Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub(crate) struct NamedCheckpoint {
    hash: String,
    name: String,
    time_ms: u64,
}

fn checkpoint_names_path(ws: &Path) -> PathBuf {
    ws.join(".ai").join("checkpoints.json")
}

/// Load the name registry (`{workspace}/.ai/checkpoints.json`).
async fn load_named_checkpoints(ws: &Path) -> Vec<NamedCheckpoint> {
    tokio::fs::read_to_string(checkpoint_names_path(ws))
        .await
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

/// Persist the name registry, creating `.ai` as needed.
async fn save_named_checkpoints(ws: &Path, list: &[NamedCheckpoint]) -> Result<(), String> {
    let dir = ws.join(".ai");
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| format!("Failed to create {}: {e}", dir.display()))?;
    let text = serde_json::to_string_pretty(list).map_err(|e| e.to_string())?;
    tokio::fs::write(checkpoint_names_path(ws), text)
        .await
        .map_err(|e| e.to_string())
}

/// Full HEAD hash in `ws`, after a checkpoint commit wrote it. Best-effort:
/// anything that prevents git from answering yields `None` and the name is
/// simply not recorded.
async fn git_head_hash(ws: &Path) -> Option<String> {
    let mut cmd = tokio::process::Command::new("git");
    cmd.args(["rev-parse", "HEAD"])
        .current_dir(ws)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    #[cfg(windows)]
    {
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let out = cmd.output().await.ok()?;
    if !out.status.success() {
        return None;
    }
    let hash = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if hash.is_empty() {
        None
    } else {
        Some(hash)
    }
}

/// Create a git checkpoint commit directly from the UI (bypasses the agent
/// tool loop; safe, additive — `git add -A` + `git commit`). An optional
/// `name` records a friendly label for the snapshot in
/// `.ai/checkpoints.json` (backward-compatible: unnamed checkpoints are
/// simply not registered, and a name-record failure never fails the commit).
#[tauri::command]
pub async fn agent_git_checkpoint_cmd(
    state: State<'_, Arc<ToolState>>,
    interrupt_state: State<'_, InterruptState>,
    message: Option<String>,
    name: Option<String>,
) -> Result<ToolResult, String> {
    let result = git_checkpoint(&state, &interrupt_state.current(), message.as_deref()).await?;
    let label = name.map(|n| n.trim().to_string()).filter(|n| !n.is_empty());
    if result.success {
        if let Some(label) = label {
            if let Some(ws) = state.primary_workspace().await {
                if let Some(hash) = git_head_hash(&ws).await {
                    let mut list = load_named_checkpoints(&ws).await;
                    list.retain(|c| c.hash != hash);
                    list.push(NamedCheckpoint {
                        hash,
                        name: label,
                        time_ms: now_ms(),
                    });
                    if let Err(e) = save_named_checkpoints(&ws, &list).await {
                        crate::logging::warn(
                            None,
                            "checkpoint",
                            &format!("checkpoint created but name not persisted: {e}"),
                        );
                    }
                }
            }
        }
    }
    Ok(result)
}

/// List existing checkpoints (hash/subject/relative age/user name), newest
/// first, for the one-click revert UI.
#[tauri::command]
pub async fn agent_git_checkpoints_cmd(
    state: State<'_, Arc<ToolState>>,
    interrupt_state: State<'_, InterruptState>,
) -> Result<Vec<CheckpointInfo>, String> {
    let ws = state.primary_workspace().await;
    let names: HashMap<String, String> = match ws.as_deref() {
        Some(ws) => load_named_checkpoints(ws)
            .await
            .into_iter()
            .map(|c| (c.hash, c.name))
            .collect(),
        None => HashMap::new(),
    };
    let values = git_checkpoints(&state, &interrupt_state.current()).await?;
    Ok(values
        .into_iter()
        .map(|v| {
            let hash = v
                .get("hash")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let name = names.get(&hash).cloned();
            CheckpointInfo {
                hash,
                subject: v
                    .get("subject")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                relative: v
                    .get("relative")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                name,
            }
        })
        .collect())
}

/// Browse user-named snapshots (`{workspace}/.ai/checkpoints.json`), newest
/// first. Unnamed git checkpoints are excluded (see `agent_git_checkpoints_cmd`
/// for the full list).
#[tauri::command]
pub async fn agent_checkpoint_names_cmd(
    state: State<'_, Arc<ToolState>>,
) -> Result<Vec<NamedCheckpoint>, String> {
    let mut list = match state.primary_workspace().await {
        Some(ws) => load_named_checkpoints(&ws).await,
        None => Vec::new(),
    };
    list.sort_by_key(|c| std::cmp::Reverse(c.time_ms));
    Ok(list)
}

/// Hard-reset to a checkpoint commit. Destructive by nature — the frontend
/// confirms before calling; the workspace policy's `git_revert` ask-rule does
/// not apply here because this is an explicit user action, not a model call.
#[tauri::command]
pub async fn agent_git_revert_cmd(
    state: State<'_, Arc<ToolState>>,
    interrupt_state: State<'_, InterruptState>,
    commit: Option<String>,
) -> Result<ToolResult, String> {
    git_revert(&state, &interrupt_state.current(), commit.as_deref()).await
}

// ---------------------------------------------------------------------------
// Proposed-edit review (Cursor/Claude-style chat + apply pipeline)
// ---------------------------------------------------------------------------

/// Every proposed-edit batch currently waiting on the user's review, for the
/// review pane (the "Review changes" panel).
#[tauri::command]
pub async fn agent_list_pending_edits(
    state: State<'_, Arc<ToolState>>,
) -> Result<Vec<PendingEditGroup>, String> {
    let mut groups: Vec<PendingEditGroup> = list_pending_edits(&state).into_values().collect();
    groups.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(groups)
}

/// Apply an approved edit batch (or only the selected edits within it):
/// conflict-check each file against its proposed snapshot, take a pre-apply
/// checkpoint, write, then unblock the proposing tool. Returns final statuses.
#[tauri::command]
pub async fn agent_apply_edits(
    app: AppHandle,
    state: State<'_, Arc<ToolState>>,
    group_id: String,
    edit_ids: Option<Vec<String>>,
) -> Result<Vec<FileEdit>, String> {
    let ctx = tool_ctx(&app);
    apply_edits(&ctx, &state, &group_id, &edit_ids.unwrap_or_default()).await
}

/// Reject an edit batch (or selected edits) and unblock the proposing tool so
/// it can self-correct.
#[tauri::command]
pub async fn agent_reject_edits(
    app: AppHandle,
    state: State<'_, Arc<ToolState>>,
    group_id: String,
    edit_ids: Option<Vec<String>>,
    reason: Option<String>,
) -> Result<Vec<FileEdit>, String> {
    let ctx = tool_ctx(&app);
    reject_edits(
        &state,
        &ctx,
        &group_id,
        &edit_ids.unwrap_or_default(),
        reason,
    )
    .await
}

/// Resolve conflict-status edits in one group: `overwrite=true` force-writes
/// the proposed content (checkpointed like a normal apply); `overwrite=false`
/// ("reload from disk") keeps the on-disk file and rejects the proposal so the
/// model re-proposes against the current content.
#[tauri::command]
pub async fn agent_resolve_edit_conflict(
    app: AppHandle,
    state: State<'_, Arc<ToolState>>,
    group_id: String,
    overwrite: bool,
    edit_ids: Option<Vec<String>>,
) -> Result<Vec<FileEdit>, String> {
    let ctx = tool_ctx(&app);
    resolve_conflict_edits(
        &ctx,
        &state,
        &group_id,
        overwrite,
        &edit_ids.unwrap_or_default(),
    )
    .await
}

/// Run the post-edit verify harness on demand (`.ai/verify.json` or a
/// per-language default) for the given files, emitting `agent://verify-result`.
#[tauri::command]
pub async fn agent_run_verify(
    app: AppHandle,
    state: State<'_, Arc<ToolState>>,
    files: Option<Vec<String>>,
) -> Result<crate::agent::verify::VerifyRun, String> {
    let workspace = state
        .primary_workspace()
        .await
        .ok_or_else(|| "No workspace set".to_string())?;
    let files = files.unwrap_or_default();
    let run = crate::agent::verify::run(&workspace, &files, Some(&state.lsp))
        .await
        .ok_or_else(|| {
            "Nothing to verify (no .ai/verify.json and no matching default check)".to_string()
        })?;
    let _ = app.emit(
        "agent://verify-result",
        crate::agent::VerifyResultEvent {
            session_id: state.session_id.load(std::sync::atomic::Ordering::SeqCst),
            ok: run.ok,
            command: run.command.clone(),
            exit_code: run.exit_code,
            output: run.report.clone(),
            timestamp_ms: crate::agent::now_ms(),
        },
    );
    Ok(run)
}

/// Pre-apply file snapshots taken during the session (`checkpoint_restore` can
/// roll a file back), newest first.
#[tauri::command]
pub async fn agent_checkpoint_list(
    state: State<'_, Arc<ToolState>>,
) -> Result<Vec<FileCheckpoint>, String> {
    Ok(list_checkpoints(&state))
}

/// Restore one or more files from their pre-apply snapshots (per file or batch).
#[tauri::command]
pub async fn agent_checkpoint_restore(
    app: AppHandle,
    state: State<'_, Arc<ToolState>>,
    ids: Option<Vec<String>>,
) -> Result<usize, String> {
    let ctx = tool_ctx(&app);
    restore_checkpoints(&ctx, &state, &ids.unwrap_or_default()).await
}
