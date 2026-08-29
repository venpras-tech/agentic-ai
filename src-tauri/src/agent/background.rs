//! Background agent tasks (P2-12).
//!
//! Background tasks run the same agent loop as foreground tasks but with
//! independent cancellation tokens and lifecycle tracking. Token/step/tool
//! events flow through the same channels as foreground tasks (the frontend
//! filters by `sessionId` to keep background turns separate from the chat).

use crossbeam_channel::Sender;
use tauri::AppHandle;
use tauri::Emitter;

use super::context::ContextMessage;
use super::interrupt::InterruptState;
use super::orchestrator::{run_agent_loop_pool, AgentTaskRequest};
use super::{BackgroundTaskEvent, ToolState};
use crate::engine::{EnginePool, WorkerEvent};
use crate::logging;

/// Start a background agent task. The task runs on a dedicated native thread
/// with its own cancellation token, completely independent of the foreground
/// interrupt state. Returns the session id.
pub fn start_background_task(
    pool: std::sync::Arc<EnginePool>,
    tool_state: &std::sync::Arc<ToolState>,
    app: &AppHandle,
    interrupt_state: &InterruptState,
    tx: &Sender<WorkerEvent>,
    request: &AgentTaskRequest,
    context_messages: &[ContextMessage],
    context_budget: usize,
) -> Result<u64, String> {
    let session_id = interrupt_state.next_session();
    let label: String = request.prompt.chars().take(48).collect();

    let (task_id, cancel) = tool_state
        .background_tasks
        .register(session_id, label.clone());

    logging::info(
        Some(session_id),
        "llm.request",
        &format!(
            "background task bg-{} · {} chars · max_steps={}",
            &task_id[3..],
            request.prompt.chars().count(),
            request.max_steps.unwrap_or(6),
        ),
    );

    let app_clone = app.clone();
    let tool_state_arc = std::sync::Arc::clone(tool_state);
    let context_snapshot = context_messages.to_vec();
    let request_clone = request.clone();
    let bg_task_id = task_id.clone();
    let tx_clone = tx.clone();

    // Emit "started" immediately.
    let _ = app_clone.emit(
        "agent://bg-task-event",
        BackgroundTaskEvent {
            task_id: task_id.clone(),
            session_id,
            label: label.clone(),
            status: "started".into(),
            detail: None,
        },
    );

    std::thread::spawn(move || {
        let result = run_agent_loop_pool(
            &pool,
            &tool_state_arc,
            &app_clone,
            &cancel,
            &tx_clone,
            session_id,
            &context_snapshot,
            &request_clone,
            context_budget,
        );

        match result {
            Ok(outcome) => {
                let _ = tx_clone.send(WorkerEvent::Done {
                    session_id,
                    done: outcome.done,
                });
                tool_state_arc
                    .background_tasks
                    .finish(&bg_task_id, "completed", None);
                let _ = app_clone.emit(
                    "agent://bg-task-event",
                    BackgroundTaskEvent {
                        task_id: bg_task_id,
                        session_id,
                        label,
                        status: "completed".into(),
                        detail: None,
                    },
                );
            }
            Err(message) => {
                let _ = tx_clone.send(WorkerEvent::Error {
                    session_id,
                    message: message.clone(),
                });
                tool_state_arc
                    .background_tasks
                    .finish(&bg_task_id, "error", Some(message.clone()));
                let _ = app_clone.emit(
                    "agent://bg-task-event",
                    BackgroundTaskEvent {
                        task_id: bg_task_id,
                        session_id,
                        label,
                        status: "error".into(),
                        detail: Some(message),
                    },
                );
            }
        }
    });

    Ok(session_id)
}
