//! Context eviction-queue control (`ContextState`/`agent::context`): token
//! budget reporting, system-prompt and file-buffer pinning, and appending
//! evictable conversation turns. Moved out of `main.rs`.

use tauri::State;

use crate::agent::context::UsageReport;
use crate::ContextState;

/// Current token-budget report (total, limit, 80% threshold, evictions).
#[tauri::command]
pub async fn context_status(state: State<'_, ContextState>) -> Result<UsageReport, String> {
    Ok(state.inner.lock().await.usage())
}

/// Pin (or replace) the system prompt; it is never evicted.
#[tauri::command]
pub async fn context_set_system_prompt(
    state: State<'_, ContextState>,
    content: String,
) -> Result<UsageReport, String> {
    let mut inner = state.inner.lock().await;
    inner.set_system_prompt(content);
    Ok(inner.usage())
}

/// Pin (or replace) the active-file context buffer; it is never evicted.
#[tauri::command]
pub async fn context_set_file_buffer(
    state: State<'_, ContextState>,
    content: String,
) -> Result<UsageReport, String> {
    let mut inner = state.inner.lock().await;
    inner.set_file_buffer(content);
    Ok(inner.usage())
}

/// Pin an editor code selection with its location; it stays in context like
/// the file buffer and survives sliding-window eviction. Re-pinning with the
/// same path replaces the previous selection slot. Returns the updated report.
#[tauri::command]
pub async fn context_set_selection(
    state: State<'_, ContextState>,
    path: String,
    start_line: Option<u64>,
    end_line: Option<u64>,
    content: String,
) -> Result<UsageReport, String> {
    let mut inner = state.inner.lock().await;
    let range = match (start_line, end_line) {
        (Some(s), Some(e)) if e > s => format!(":{s}-{e}"),
        (Some(s), None) => format!(":{s}"),
        _ => String::new(),
    };
    let labeled = format!("### {path}{range}\n{content}");
    inner.upsert_pinned("selection", labeled);
    Ok(inner.usage())
}

/// Append an evictable conversation turn; auto-triggers sliding-window
/// truncation when usage crosses 80% of the model's context limit.
#[tauri::command]
pub async fn context_push_turn(
    state: State<'_, ContextState>,
    role: String,
    content: String,
) -> Result<UsageReport, String> {
    let mut inner = state.inner.lock().await;
    Ok(inner.push(&role, content))
}
