//! Native tool implementations + the unified dispatcher (`tools::dispatch`).
//!
//! Every [`ToolCall`](super::ToolCall) variant that the orchestrator hands to
//! us is executed here, on-device, with zero SaaS dependencies. The dispatcher
//! is the single chokepoint for:
//!
//!   1. **Policy** — [`policy::check`] decides allow/ask/deny per call before
//!      anything runs; ask-verdicts are triaged to the UI over
//!      `agent://permission-request` and the user's `PermissionDecision` is
//!      honoured (once / session / always).
//!   2. **Audit** — every call (allowed *and* denied) is appended to
//!      `{workspace}/.ai/audit.jsonl` so the UI can render the policy trail.
//!   3. **Transparency** — every call emits start/done/error events on
//!      `agent://tool-event` that the React timeline renders in real time.
//!
//! Tools return `Result<ToolResult, String>`; the dispatcher flattens any
//! error into a `ToolResult{success:false}` so the agent loop can self-correct
//! without ever crashing the app.

use std::collections::{HashMap, HashSet};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use super::ToolContext;
use globset::{GlobBuilder, GlobSet};
use ignore::WalkBuilder;
use regex::Regex;
use serde_json::{json, Value};
use streaming_iterator::StreamingIterator;
use tokio_util::sync::CancellationToken;
use tree_sitter::{Language, Node, Parser, Query, QueryCursor};

use super::edits::{propose_edit, FileEditSpec};
use super::{
    hooks, mcp, now_ms, plan, policy, rag, skills, todo, AgentToolEvent, HitlGateEvent,
    PermissionDecision, PermissionRequestEvent, QuestionRequestEvent, TodoUpdateEvent, ToolCall,
    ToolResult, ToolState,
};
use crate::engine::{ChatTurn, InferenceRequest, TextGenerator, WorkerEvent};

// ---------------------------------------------------------------------------
// Dispatcher chokepoint
// ---------------------------------------------------------------------------

/// Execute one tool call: policy gate → triage → run → event + audit.
pub async fn dispatch(
    app: &ToolContext,
    state: &ToolState,
    call: &ToolCall,
    interrupt: CancellationToken,
) -> Result<ToolResult, String> {
    if interrupt.is_cancelled() {
        return Err("Execution Aborted".into());
    }

    let started_at = now_ms();
    let id = state.next_event_id();
    let tool = call.name();
    let summary = call.summary();

    emit_tool_event(
        app, state, &id, tool, "running", &summary, started_at, None, None,
    );

    // P3.1 workspace hooks: a user-defined `pre_tool_use` command can veto the
    // call before policy sees it (Claude PreToolUse parity). Fail-open by
    // design — a broken/missing hooks.json never blocks the agent.
    let workspaces = state.workspace.lock().await.clone();
    let primary = workspaces.first().cloned();
    if let Some(ws) = primary.as_deref() {
        let cfg = hooks::load(ws);
        if !cfg.pre_tool_use.is_empty() {
            let session = state.session_id.load(std::sync::atomic::Ordering::SeqCst);
            let payload = hooks::payload("pre_tool_use", ws, Some(call), session);
            if let hooks::HookOutcome::Deny(reason) =
                hooks::run_hooks(ws, &cfg.pre_tool_use, &payload, Some(tool)).await
            {
                let elapsed = started_at.max(now_ms()) - started_at;
                emit_tool_event(
                    app,
                    state,
                    &id,
                    tool,
                    "error",
                    &summary,
                    started_at,
                    Some(elapsed),
                    Some(reason.clone()),
                );
                audit(
                    state,
                    primary.as_deref(),
                    &id,
                    tool,
                    &summary,
                    "hook-deny",
                    started_at,
                    elapsed,
                    Some(false),
                    Some(&reason),
                );
                let _ = app.emit(
                    "agent://hook",
                    serde_json::json!({
                        "event": "pre_tool_use",
                        "tool": tool,
                        "status": "deny",
                        "detail": reason.clone(),
                        "sessionId": session,
                    }),
                );
                return Ok(ToolResult::err(
                    tool,
                    "denied by pre_tool_use hook".into(),
                    reason,
                ));
            }
        }
    }

    // `ask_question` / `send_to_user` are themselves the interaction primitive —
    // they never require a further permission gate.
    let outcome = if matches!(
        call,
        ToolCall::AskQuestion { .. } | ToolCall::SendToUser { .. }
    ) {
        (
            "allow".to_string(),
            run_tool(app, state, call, &interrupt).await,
        )
    } else {
        let verdict = policy::check(state, call, &workspaces);
        match verdict {
            policy::Verdict::Allow => (
                "allow".to_string(),
                run_tool(app, state, call, &interrupt).await,
            ),
            policy::Verdict::Deny(reason) => (
                "deny".to_string(),
                Ok(ToolResult::err(tool, "denied".into(), reason)),
            ),
            policy::Verdict::Ask { request_id } => {
                triage_permission(app, state, call, primary.as_deref(), request_id, &summary).await
            }
            policy::Verdict::AskFolderDelete { request_id, detail } => {
                // Folder deletes and plain asks share the same triage; the
                // extra detail string is rendered by the UI prompt header.
                let _ = detail;
                triage_permission(app, state, call, primary.as_deref(), request_id, &summary).await
            }
        }
    };

    let elapsed = started_at.max(now_ms()) - started_at;
    let (decision, mut result) = outcome;
    result = result.map(|mut r| {
        if r.duration_ms == 0 {
            r.duration_ms = elapsed;
        }
        r
    });

    let (status, detail) = match &result {
        Ok(r) if r.success => ("done", r.error.clone()),
        Ok(r) => ("error", r.error.clone()),
        Err(e) => ("error", Some(e.clone())),
    };
    let exit_code = result
        .as_ref()
        .ok()
        .and_then(|r| r.stats.as_ref())
        .and_then(|s| s.get("exit_code"))
        .and_then(|v| v.as_i64())
        .map(|v| v as i32);
    emit_tool_event_full(
        app,
        state,
        &id,
        tool,
        status,
        &summary,
        started_at,
        Some(elapsed),
        exit_code,
        detail,
    );

    // Deliver the tool's captured output to the UI so the mini terminal drawer
    // can render it with the completion event (the `agent://tool-output` event
    // payload carries stdout so the frontend never needs the full ToolResult).
    if let Ok(r) = &result {
        if let Some(out) = r.stdout.as_ref() {
            if !out.is_empty() {
                let _ = app.emit(
                    "agent://tool-output",
                    serde_json::json!({
                        "id": id,
                        "tool": tool,
                        "status": status,
                        "output": out,
                        "sessionId": state.session_id.load(std::sync::atomic::Ordering::SeqCst),
                    }),
                );
            }
        }
    }

    // P3.1 post_tool_use: notify the user's hook of the completed call. The
    // hook cannot block (the tool already ran) — its stdout is ignored.
    if let Some(ws) = primary.as_deref() {
        let cfg = hooks::load(ws);
        if !cfg.post_tool_use.is_empty() {
            let session = state.session_id.load(std::sync::atomic::Ordering::SeqCst);
            let mut payload = hooks::payload("post_tool_use", ws, Some(call), session);
            let (ok, err_text, out_text) = match &result {
                Ok(r) => (r.success, r.error.clone(), r.stdout.clone()),
                Err(e) => (false, Some(e.clone()), None),
            };
            payload =
                hooks::with_tool_output(payload, ok, err_text.as_deref(), out_text.as_deref());
            let _ = hooks::run_hooks(ws, &cfg.post_tool_use, &payload, Some(tool)).await;
        }
    }

    let (ok, err_text) = match &result {
        Ok(r) => (Some(r.success), r.error.clone()),
        Err(e) => (Some(false), Some(e.clone())),
    };
    audit(
        state,
        primary.as_deref(),
        &id,
        tool,
        &summary,
        &decision,
        started_at,
        elapsed,
        ok,
        err_text.as_deref(),
    );

    result
}

/// Triage a policy-`ask` verdict: emit a permission request, await the user's
/// decision (over `agent_respond_permission`), then run or refuse.
#[allow(clippy::too_many_arguments)]
async fn triage_permission(
    app: &ToolContext,
    state: &ToolState,
    call: &ToolCall,
    primary: Option<&Path>,
    request_id: String,
    summary: &str,
) -> (String, Result<ToolResult, String>) {
    let review = review_command(app, state, call).await;

    let (tx, rx) = tokio::sync::oneshot::channel();
    state
        .permission_requests
        .lock()
        .await
        .insert(request_id.clone(), tx);
    let _ = app.emit(
        "agent://permission-request",
        PermissionRequestEvent {
            request_id: request_id.clone(),
            tool: call.name().to_string(),
            summary: summary.to_string(),
            timestamp_ms: now_ms(),
            review,
        },
    );

    let decision = tokio::time::timeout(Duration::from_secs(300), rx).await;

    let decision = match decision {
        Ok(Ok(d)) => d,
        _ => {
            // Timed out (or the sender dropped): clean up the pending entry.
            state.permission_requests.lock().await.remove(&request_id);
            return (
                "timed-out".into(),
                Ok(ToolResult::err(
                    call.name(),
                    "Timed out waiting for approval".into(),
                    "The permission request timed out before the user answered.".into(),
                )),
            );
        }
    };

    match decision {
        PermissionDecision::AllowOnce => (
            "granted".into(),
            run_tool(app, state, call, &CancellationToken::new()).await,
        ),
        PermissionDecision::AllowSession => {
            policy::remember_session(state, call);
            // Persist the grant so a restart doesn't force re-approval. The
            // legacy path saved here; the command rewrite left this load-only.
            state.save_session_allow();
            (
                "granted-session".into(),
                run_tool(app, state, call, &CancellationToken::new()).await,
            )
        }
        PermissionDecision::AlwaysAllow => {
            let _ = policy::remember_always(primary, call);
            (
                "granted-always".into(),
                run_tool(app, state, call, &CancellationToken::new()).await,
            )
        }
        PermissionDecision::Deny => (
            "declined".into(),
            Ok(ToolResult::err(
                call.name(),
                "User denied".into(),
                "The user declined this action.".into(),
            )),
        ),
    }
}

/// Best-effort independent LLM review of a terminal command (Bionic §3.3
/// hardening): a one-line second opinion rendered next to the approve buttons.
/// Returns `None` when no pooled engine is loaded or the review fails — the
/// prompt is simply shown without a review line.
async fn review_command(app: &ToolContext, state: &ToolState, call: &ToolCall) -> Option<String> {
    let ToolCall::ExecuteTerminalCommand { command, .. } = call else {
        return None;
    };
    let review = llm_brief(
        state,
        "You are a security reviewer. Judge the single terminal command and answer in \
         one short line: exactly 'ROUTINE' if it is a normal read/build/edit command, \
         otherwise 'CAREFUL: <one reason it is risky>'. Never be verbose.",
        &format!("Command: {command}\nJudgment:"),
        48,
    )
    .await?;
    let trimmed = review.trim().to_string();
    if trimmed.is_empty() {
        None
    } else {
        let _ = app;
        Some(trimmed)
    }
}

/// One-shot non-streaming generation on the pool's reserved worker. Used by
/// LLM-backed tools (`analyze_bug`, `review_code`, `summarize_changes`,
/// permission reviews). Returns `None` when no model is loaded or it fails —
/// callers degrade gracefully instead of erroring to the model.
async fn llm_brief(state: &ToolState, system: &str, user: &str, max_tokens: u32) -> Option<String> {
    let pool = state.engine.lock().await.clone()?;
    let mut gen = pool.handle(0);
    let session_id = state.session_id.load(std::sync::atomic::Ordering::SeqCst);
    let request = InferenceRequest {
        prompt: user.to_string(),
        messages: Some(vec![
            ChatTurn {
                role: "system".into(),
                content: system.to_string(),
            },
            ChatTurn {
                role: "user".into(),
                content: user.to_string(),
            },
        ]),
        images: None,
        max_tokens,
        stop_words: None,
        cached_prefix_tokens: None,
        ..Default::default()
    };
    let (tx, _rx) = crossbeam_channel::unbounded::<WorkerEvent>();
    let itok = CancellationToken::new();
    // `generate` is CPU-bound (llama.cpp); keep it off the async runtime so a
    // brief doesn't stall the agent loop for other tasks.
    let generated =
        tokio::task::spawn_blocking(move || gen.generate(&request, session_id, &itok, &tx)).await;
    match generated {
        Ok(Ok(out)) => {
            let t = out.full_text.trim().to_string();
            if t.is_empty() {
                None
            } else {
                Some(t)
            }
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Events + audit
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn emit_tool_event(
    app: &ToolContext,
    state: &ToolState,
    id: &str,
    tool: &str,
    status: &str,
    summary: &str,
    started_at: u64,
    duration_ms: Option<u64>,
    detail: Option<String>,
) {
    emit_tool_event_full(
        app, state, id, tool, status, summary, started_at, duration_ms, None, detail,
    );
}

#[allow(clippy::too_many_arguments)]
fn emit_tool_event_full(
    app: &ToolContext,
    state: &ToolState,
    id: &str,
    tool: &str,
    status: &str,
    summary: &str,
    started_at: u64,
    duration_ms: Option<u64>,
    exit_code: Option<i32>,
    detail: Option<String>,
) {
    let session_id = state.session_id.load(std::sync::atomic::Ordering::SeqCst);
    // Mirror the tool lifecycle into the background-task output ring buffer
    // (unbound sessions are ignored by the store).
    state
        .background_output
        .record(session_id, &format!("[{tool}] {status} · {summary}"));
    let _ = app.emit(
        "agent://tool-event",
        AgentToolEvent {
            id: id.to_string(),
            tool: tool.to_string(),
            status: status.to_string(),
            summary: summary.to_string(),
            started_at,
            duration_ms,
            exit_code,
            detail,
            session_id,
        },
    );
}

fn emit_todo_update(app: &ToolContext, list: &todo::TodoList) {
    let _ = app.emit(
        "agent://todo-update",
        TodoUpdateEvent {
            items: list.items.clone(),
            updated_at: list.updated_at,
        },
    );
}

/// Append one decision record to `{workspace}/.ai/audit.jsonl` (camelCase,
/// matching the frontend's `AuditEntry`). Best-effort: never fails the call.
pub fn audit(
    state: &ToolState,
    workspace: Option<&Path>,
    id: &str,
    tool: &str,
    summary: &str,
    decision: &str,
    started_at: u64,
    latency_ms: u64,
    success: Option<bool>,
    error: Option<&str>,
) {
    let _ = state;
    let Some(ws) = workspace else { return };
    let dir = ws.join(".ai");
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let path = dir.join("audit.jsonl");
    let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    else {
        return;
    };
    let entry = json!({
        "ts": now_ms(),
        "id": id,
        "tool": tool,
        "summary": summary,
        "decision": decision,
        "startedAt": started_at,
        "latencyMs": latency_ms,
        "success": success,
        "error": error,
    });
    let _ = writeln!(f, "{entry}");
}

/// Number of still-open todo items for the current workspace (orchestrator
/// uses it to bounce the model back until the list is closed).
pub async fn open_todo_count(state: &ToolState) -> Result<usize, String> {
    let ws = state
        .primary_workspace()
        .await
        .ok_or_else(|| "No workspace set".to_string())?;
    Ok(todo::TodoList::load(&ws)
        .map(|l| l.open_count())
        .unwrap_or(0))
}

// ---------------------------------------------------------------------------
// Core executor
// ---------------------------------------------------------------------------

/// The monolithic matcher: one arm per [`ToolCall`] variant. Prepares the
/// workspace root, then delegates to the per-tool functions below.
async fn run_tool(
    app: &ToolContext,
    state: &ToolState,
    call: &ToolCall,
    interrupt: &CancellationToken,
) -> Result<ToolResult, String> {
    let ws = state.workspace.lock().await.clone();
    let root = ws.first().cloned();
    let name = call.name();

    // File-mutating tools auto-checkpoint once per agent step.
    let mutates = matches!(
        call,
        ToolCall::WriteFile { .. } | ToolCall::ApplyFileDiff { .. }
    );

    let result = match call {
        ToolCall::GlobSearchCodebase { pattern, root, respect_gitignore } => {
            let base = root.as_deref().map(PathBuf::from).or_else(|| ws.first().cloned()).unwrap_or_default();
            glob_search(&base, pattern, respect_gitignore.unwrap_or(true)).await
        }
        ToolCall::SearchFileContents { pattern, include, root, respect_gitignore } => {
            let base = root.as_deref().map(PathBuf::from).or_else(|| ws.first().cloned()).unwrap_or_default();
            search_file_contents(&base, pattern, include.as_deref(), respect_gitignore.unwrap_or(true)).await
        }
        ToolCall::ViewFileStructure { path, max_depth } => {
            let p = resolve(root.as_deref(), path);
            Ok(ToolResult::ok(name, format!("Parsed AST of `{}`", display(path)), Some(ast_outline(&p, max_depth.unwrap_or(6))?), None))
        }
        ToolCall::ReadFileRange { path, start_line, end_line } => {
            let p = resolve(root.as_deref(), path);
            read_file_range(&p, *start_line, *end_line)
        }
        ToolCall::ApplyFileDiff { path, diff } => {
            let p = resolve(root.as_deref(), path);
            apply_diff_tool(app, state, &p, diff).await
        }
        ToolCall::ExecuteTerminalCommand { command, timeout_secs, cwd } => {
            let dir = cwd
                .as_deref()
                .map(|c| resolve(root.as_deref(), c))
                .or_else(|| root.clone());
            execute_terminal_command(command, dir.as_deref(), timeout_secs.or(Some(120)), interrupt).await
        }
        ToolCall::CallMcpTool { server, server_bin, server_args, tool, arguments, timeout_secs } => {
            call_mcp_tool(app, state, ws.as_slice(), tool, arguments, *timeout_secs, server, server_bin, server_args).await
        }
        ToolCall::ListMcpServers { .. } => list_mcp_servers(app),
        ToolCall::AddMcpServer { name, bin, args } => {
            add_mcp_server(app, name, bin, args).await.map(|servers| ToolResult::ok(name, format!("Added MCP server `{name}`"), Some(serde_json::to_string_pretty(&servers).unwrap_or_default()), None))
        }
        ToolCall::RemoveMcpServer { name } => {
            remove_mcp_server(app, name).await
        }
        ToolCall::AttachFile { path } => {
            let p = resolve(root.as_deref(), path);
            attach_file(state, &p).map(|a| ToolResult::ok(name, format!("Indexed `{}` ({} chunks)", display(path), a.chunk_count), None, None))
        }
        ToolCall::SearchAttachedFiles { query, top_k } => {
            let k = top_k.unwrap_or(5).min(20);
            search_attached(state, query, k)
        }
        ToolCall::DetachFile { path } => {
            let p = resolve(root.as_deref(), path);
            let removed = state.rag.lock().unwrap().detach(p.to_str().unwrap_or_default());
            Ok(ToolResult::ok(name, if removed { format!("Detached `{}`", display(path)) } else { format!("`{}` was not attached", display(path)) }, None, None))
        }
        ToolCall::TranscribeAudio { path, language } => {
            let p = resolve(root.as_deref(), path);
            transcribe_audio_tool(&p, language.as_deref()).await
        }
        ToolCall::GitStatus { .. } => git_status(&ws).await,
        ToolCall::GitDiff { path } => git_diff(&ws, path.as_deref()).await,
        ToolCall::GitCommit { message } => git_commit(&ws, message).await,
        ToolCall::GitCheckpoint { message } => git_checkpoint(state, interrupt, message.as_deref()).await,
        ToolCall::GitRevert { commit } => git_revert(state, interrupt, commit.as_deref()).await,
        ToolCall::GitBlame { path, start_line, end_line } => {
            let p = resolve(root.as_deref(), path);
            git_blame(&ws, &p, *start_line, *end_line).await
        }
        ToolCall::GitPush { remote, branch, set_upstream } => {
            git_push(&ws, remote.as_deref(), branch.as_deref(), set_upstream.unwrap_or(false)).await
        }
        ToolCall::GitPull { .. } => git_pull(&ws).await,
        ToolCall::GitCreateBranch { name } => git_create_branch(&ws, name).await,
        ToolCall::GitPrStatus { .. } => git_pr_status(&ws).await,
        ToolCall::GitCiStatus { .. } => git_ci_status(&ws).await,
        ToolCall::CreatePr { title, body } => create_pr(&ws, title, body.as_deref()).await,
        ToolCall::RunTests { command } => run_tests(first_ws(&ws).unwrap_or(Path::new(".")), command.as_deref(), interrupt).await,
        ToolCall::WriteFile { path, content } => {
            let p = resolve(root.as_deref(), path);
            write_file(app, state, &p, content).await
        }
        ToolCall::CreateSkill { name, description, content } => create_skill(state, name, description.as_deref(), content),
        ToolCall::ReadSkill { name } => read_skill(state, name),
        ToolCall::SuggestSkills { prompt, path } => suggest_skills(state, prompt, path.as_deref()),
        ToolCall::SemanticSearchCodebase { query, include, root, respect_gitignore, top_k } => {
            let base = root.as_deref().map(PathBuf::from).or_else(|| ws.first().cloned()).unwrap_or_default();
            semantic_search(state, &base, query, include.as_deref(), respect_gitignore.unwrap_or(true), top_k.unwrap_or(8)).await
        }
        ToolCall::CreatePlan { title, goal, items } => create_plan(state, root.as_deref(), title, goal, items),
        ToolCall::ReadPlan { .. } => read_plan(state, root.as_deref()),
        ToolCall::UpdatePlan { item, status, details } => update_plan(state, root.as_deref(), *item, status, details.as_deref()),
        ToolCall::ExecutePlan { .. } => Ok(ToolResult::err(
            name,
            "Handled by the orchestrator".into(),
            "`execute_plan` is driven by the agent loop, not the generic dispatcher.".into(),
        )),
        ToolCall::ListDir { path } => {
            let p = path
                .as_deref()
                .map(|p| resolve(root.as_deref(), p))
                .or(root.clone())
                .unwrap_or_else(|| PathBuf::from("."));
            list_dir(&p)
        }
        ToolCall::ReadFileChars { path, offset, limit } => {
            let p = resolve(root.as_deref(), path);
            read_file_chars(&p, *offset, *limit)
        }
        ToolCall::CreateFolder { path } => {
            let p = resolve(root.as_deref(), path);
            create_folder(&p)
        }
        ToolCall::CopyFileOrFolder { src, dst, can_overwrite } => {
            let s = resolve(root.as_deref(), src);
            let d = resolve(root.as_deref(), dst);
            copy_path(&s, &d, can_overwrite.unwrap_or(false))
        }
        ToolCall::MoveFileOrFolder { src, dst, can_overwrite } => {
            let s = resolve(root.as_deref(), src);
            let d = resolve(root.as_deref(), dst);
            move_path(&s, &d, can_overwrite.unwrap_or(false))
        }
        ToolCall::DeleteFileOrFolder { path } => {
            let p = resolve(root.as_deref(), path);
            delete_path(&p)
        }
        ToolCall::GetScratchpadFolder { .. } => {
            let session = state.session_id.load(std::sync::atomic::Ordering::SeqCst);
            let dir = super::session_scratchpad(session);
            std::fs::create_dir_all(&dir)
                .map_err(|e| format!("Failed to create scratchpad: {e}"))?;
            Ok(ToolResult::ok(name, format!("Session scratchpad: {}", dir.display()), Some(dir.to_string_lossy().into_owned()), None))
        }
        ToolCall::SetTodoList { items } => {
            let list = todo::new_list(items.clone());
            let ws_root = root.ok_or_else(|| "No workspace set".to_string())?;
            list.save(&ws_root)?;
            emit_todo_update(app, &list);
            Ok(ToolResult::ok(name, format!("Todo list set ({} item(s))", list.items.len()), Some(list.render()), None))
        }
        ToolCall::GetTodoList { .. } => {
            let ws_root = root.ok_or_else(|| "No workspace set".to_string())?;
            let list = todo::TodoList::load(&ws_root).unwrap_or_default();
            Ok(ToolResult::ok(name, "Todo list".into(), Some(list.render()), None))
        }
        ToolCall::MarkTodoItemDone { item } => {
            let ws_root = root.ok_or_else(|| "No workspace set".to_string())?;
            let mut list = todo::TodoList::load(&ws_root).unwrap_or_default();
            if *item < 1 || *item > list.items.len() {
                return Err(format!("Todo item #{item} is out of range (1..={})", list.items.len()));
            }
            list.items[*item - 1].done = true;
            list.updated_at = now_ms();
            list.save(&ws_root)?;
            emit_todo_update(app, &list);
            Ok(ToolResult::ok(name, format!("Marked todo #{item} done"), Some(list.render()), None))
        }
        ToolCall::WebSearch { query, max_results } => {
            web_search(query, max_results.unwrap_or(8)).await
        }
        ToolCall::WebExtract { url } => web_extract(url).await,
        ToolCall::BrowseWeb { url, action } => {
            match action.as_deref() {
                Some("screenshot") => Err(format!("`browse_web` screenshots are not supported without a headless browser; use the fetch action instead.")),
                _ => web_extract(url).await,
            }
        }
        ToolCall::DownloadFile { url, path } => {
            let p = resolve(root.as_deref(), path);
            download_file(url, &p).await
        }
        ToolCall::RunPython { code, timeout_secs } => {
            let ws_root = root.ok_or_else(|| "No workspace set".to_string())?;
            run_interp(&ws_root, "python", code, timeout_secs.or(Some(60)), interrupt).await
        }
        ToolCall::RunJavascript { code, timeout_secs } => {
            let ws_root = root.ok_or_else(|| "No workspace set".to_string())?;
            run_interp(&ws_root, "node", code, timeout_secs.or(Some(60)), interrupt).await
        }
        ToolCall::Calculate { expression } => calculate(expression),
        ToolCall::Task { .. } => Ok(ToolResult::err(
            name,
            "Handled by the orchestrator".into(),
            "`task` runs as a first-class subagent from the agent loop, not the generic dispatcher.".into(),
        )),
        ToolCall::SummarizeChanges { .. } => summarize_changes(state, &ws).await,
        ToolCall::ReadLints { path } => {
            let p = resolve(root.as_deref(), path);
            read_lints(&p)
        }
        ToolCall::AskQuestion { question, choices } => ask_question(state, app, question, choices.as_deref()).await,
        ToolCall::SendToUser { message } => Ok(ToolResult::ok(name, message.clone(), None, None)),
        ToolCall::TreeSitterQuery { path, query, max_results } => {
            let p = resolve(root.as_deref(), path);
            tree_sitter_query(&p, query, max_results.unwrap_or(100))
        }
        ToolCall::AnalyzeBug { stack, path } => {
            let p = path.as_deref().map(|p| resolve(root.as_deref(), p));
            analyze_bug(state, root.as_deref(), stack, p.as_deref()).await
        }
        ToolCall::ReviewCode { path, diff } => {
            let p = path.as_deref().map(|p| resolve(root.as_deref(), p));
            review_code(state, root.as_deref(), p.as_deref(), diff.as_deref()).await
        }
        ToolCall::ViewRepoMap { top_n, root } => {
            let base = root.as_deref().map(PathBuf::from).or_else(|| ws.first().cloned()).unwrap_or_default();
            view_repo_map(state, &base, top_n.unwrap_or(60)).await
        }
    };

    // Auto-checkpoint the FIRST successful file edit of a step.
    if mutates {
        if let Ok(r) = &result {
            if r.success {
                maybe_auto_checkpoint(state, interrupt).await;
            }
        }
    }
    Ok(result?)
}

/// Resolve a tool-supplied path relative to the workspace root. Absolute
/// paths pass through unchanged (the policy layer already vetted them).
fn resolve(root: Option<&Path>, raw: &str) -> PathBuf {
    let p = PathBuf::from(raw);
    if p.is_absolute() {
        return p;
    }
    match root {
        Some(r) => r.join(p),
        None => p,
    }
}

fn display(path: &str) -> String {
    path.rsplit(['/', '\\']).next().unwrap_or(path).to_string()
}

// ---------------------------------------------------------------------------
// File tools
// ---------------------------------------------------------------------------

async fn glob_search(
    root: &Path,
    pattern: &str,
    respect_gitignore: bool,
) -> Result<ToolResult, String> {
    let set = build_glob_set(pattern)?;
    let mut matches: Vec<String> = Vec::new();
    let mut walker = WalkBuilder::new(root);
    walker.hidden(false).parents(false);
    if respect_gitignore {
        walker.git_ignore(true).git_exclude(true).git_global(true);
    } else {
        walker.git_ignore(false).git_exclude(false);
    }
    walker.standard_filters(respect_gitignore);
    for entry in walker.build() {
        let entry = entry.map_err(|e| format!("Walk error: {e}"))?;
        if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let rel = entry.path().strip_prefix(root).unwrap_or(entry.path());
        let rel_str = rel.to_string_lossy().replace('\\', "/");
        if set.is_match(&rel_str) || set.is_match(&format!("/{rel_str}")) {
            matches.push(rel_str);
            if matches.len() >= 1000 {
                break;
            }
        }
    }
    matches.sort();
    let total = matches.len();
    let body = if matches.is_empty() {
        "No files matched.".to_string()
    } else {
        matches.join("\n")
    };
    Ok(ToolResult::ok(
        "glob_search_codebase",
        format!("Found {total} file(s) for `{pattern}`"),
        Some(body),
        Some(json!({ "count": total })),
    ))
}

fn build_glob_set(pattern: &str) -> Result<GlobSet, String> {
    let mut builder = globset::GlobSetBuilder::new();
    let parts: Vec<&str> = pattern
        .split(|c: char| c == ' ' || c == ',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();
    if parts.is_empty() {
        return Err("Empty glob pattern".into());
    }
    for p in &parts {
        let g = GlobBuilder::new(p)
            .literal_separator(false)
            .backslash_escape(false)
            .build()
            .map_err(|e| format!("Bad glob `{p}`: {e}"))?;
        builder.add(g);
    }
    builder.build().map_err(|e| format!("Bad glob set: {e}"))
}

/// Build an include-filter GlobSet from the tool's `include` param. Absent /
/// empty means "everything".
fn include_set(include: Option<&str>) -> Option<GlobSet> {
    let include = include?.trim();
    if include.is_empty() {
        return None;
    }
    let mut builder = globset::GlobSetBuilder::new();
    for p in include.split(',') {
        let p = p.trim();
        if p.is_empty() {
            continue;
        }
        if let Ok(g) = GlobBuilder::new(p)
            .literal_separator(false)
            .backslash_escape(false)
            .build()
        {
            builder.add(g);
        }
    }
    builder.build().ok().filter(|s| !s.is_empty())
}

async fn search_file_contents(
    root: &Path,
    pattern: &str,
    include: Option<&str>,
    respect_gitignore: bool,
) -> Result<ToolResult, String> {
    let re = Regex::new(pattern).map_err(|e| format!("Invalid regex `{pattern}`: {e}"))?;
    let inc = include_set(include);
    let mut hits: Vec<String> = Vec::new();
    let mut files_scanned = 0usize;
    let mut walker = WalkBuilder::new(root);
    walker.hidden(false).parents(false);
    walker
        .git_ignore(respect_gitignore)
        .git_exclude(respect_gitignore)
        .git_global(respect_gitignore);
    walker.standard_filters(respect_gitignore);
    for entry in walker.build() {
        if hits.len() >= 500 {
            break;
        }
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        if inc.is_none() && !likely_text(entry.path()) {
            continue;
        }
        if let Some(set) = &inc {
            let rel = entry.path().strip_prefix(root).unwrap_or(entry.path());
            let rel_str = rel.to_string_lossy().replace('\\', "/");
            if !set.is_match(&rel_str) && !set.is_match(&format!("/{rel_str}")) {
                continue;
            }
        }
        let Ok(text) = std::fs::read_to_string(entry.path()) else {
            continue;
        };
        if text.len() > 2_000_000 {
            continue;
        }
        files_scanned += 1;
        let rel = entry.path().strip_prefix(root).unwrap_or(entry.path());
        for (i, line) in text.lines().enumerate() {
            if re.is_match(line) {
                hits.push(format!("{}:{}:{}", rel.to_string_lossy(), i + 1, line));
                if hits.len() >= 500 {
                    break;
                }
            }
        }
    }
    let body = if hits.is_empty() {
        "No matches found.".to_string()
    } else {
        hits.join("\n")
    };
    Ok(ToolResult::ok(
        "search_file_contents",
        format!("{} match(es) in {files_scanned} file(s)", hits.len()),
        Some(body),
        Some(json!({ "matches": hits.len(), "files": files_scanned })),
    ))
}

fn likely_text(path: &Path) -> bool {
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        let ext = ext.to_ascii_lowercase();
        if matches!(
            ext.as_str(),
            "png"
                | "jpg"
                | "jpeg"
                | "gif"
                | "webp"
                | "ico"
                | "bmp"
                | "mp3"
                | "wav"
                | "ogg"
                | "flac"
                | "mp4"
                | "webm"
                | "mov"
                | "zip"
                | "tar"
                | "gz"
                | "bz2"
                | "7z"
                | "exe"
                | "dll"
                | "so"
                | "dylib"
                | "woff"
                | "woff2"
                | "ttf"
                | "otf"
                | "pdf"
                | "bin"
        ) {
            return false;
        }
    }
    true
}

fn ts_language(path: &Path) -> Option<Language> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);
    match ext.as_deref() {
        Some("rs") => Some(tree_sitter_rust::LANGUAGE.into()),
        Some("py") => Some(tree_sitter_python::LANGUAGE.into()),
        Some("ts") | Some("mts") | Some("cts") => {
            Some(tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into())
        }
        Some("tsx") | Some("js") | Some("jsx") | Some("mjs") | Some("cjs") => {
            Some(tree_sitter_typescript::LANGUAGE_TSX.into())
        }
        Some("json") => Some(tree_sitter_json::LANGUAGE.into()),
        _ => None,
    }
}

fn parse_tree(path: &Path) -> Result<Option<(Language, tree_sitter::Tree, String)>, String> {
    let lang = match ts_language(path) {
        Some(l) => l,
        None => return Ok(None),
    };
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("Failed to read `{}`: {e}", path.display()))?;
    let mut parser = Parser::new();
    parser
        .set_language(&lang)
        .map_err(|e| format!("Unsupported language: {e}"))?;
    let tree = parser
        .parse(&text, None)
        .ok_or_else(|| format!("Failed to parse `{}`", path.display()))?;
    Ok(Some((lang, tree, text)))
}

/// Build a symbol-outline of a file using tree-sitter. Fallbacks to a raw
/// "no AST available" note for unsupported languages.
fn ast_outline(path: &Path, max_depth: usize) -> Result<String, String> {
    let (_, tree, _) = match parse_tree(path)? {
        Some(t) => t,
        None => {
            return Ok(format!(
                "No tree-sitter syntax available for `{}` (supported: rs, py, ts, tsx, js, json)",
                path.display()
            ))
        }
    };
    let mut out = String::new();

    queued_outline(&tree, path, max_depth, &mut out)?;
    Ok(out.trim_end().to_string())
}

fn queued_outline(
    tree: &tree_sitter::Tree,
    path: &Path,
    max_depth: usize,
    out: &mut String,
) -> Result<(), String> {
    let root = tree.root_node();
    let mut visited = HashSet::new();
    fn visit(
        node: &Node,
        depth: usize,
        max_depth: usize,
        out: &mut String,
        visited: &mut HashSet<usize>,
    ) {
        if depth > max_depth {
            return;
        }
        let def = match node.kind() {
            "function_declaration" => Some(("function", "function_declaration")),
            "function_item" => Some(("function", "function_item")),
            "class_declaration" => Some(("class", "class_declaration")),
            "struct_item" => Some(("struct", "struct_item")),
            "enum_item" => Some(("enum", "enum_item")),
            "trait_item" => Some(("trait", "trait_item")),
            "impl_item" => Some(("impl", "impl_item")),
            "mod_item" => Some(("module", "mod_item")),
            "function_definition" => Some(("function", "function_definition")),
            "class_definition" => Some(("class", "class_definition")),
            "method_definition" => Some(("method", "method_definition")),
            "interface_declaration" => Some(("interface", "interface_declaration")),
            "const_item" => Some(("const", "const_item")),
            "static_item" => Some(("static", "static_item")),
            "macro_definition" => Some(("macro", "macro_definition")),
            _ => None,
        };
        if let Some((label, _kind)) = def {
            if !visited.insert(node.id()) {
                return;
            }
            let name = node
                .child_by_field_name("name")
                .map(|n| n.utf8_text(&[]).unwrap_or("").to_string())
                .unwrap_or_default();
            let (start_l, start_c) = (node.start_position().row + 1, node.start_position().column);
            let indent = "  ".repeat(depth.saturating_sub(1));
            let label = if name.is_empty() {
                label.to_string()
            } else {
                format!("{label} {name}")
            };
            out.push_str(&format!("{indent}{label} @ {start_l}:{start_c}\n"));
        }
        let kind = node.kind();
        let is_container = matches!(
            kind,
            "function_declaration"
                | "function_item"
                | "struct_item"
                | "class_declaration"
                | "class_definition"
                | "enum_item"
                | "trait_item"
                | "impl_item"
                | "mod_item"
                | "function_definition"
                | "method_definition"
                | "interface_declaration"
                | "translation_unit"
                | "program"
                | "module"
                | "source_file"
        );
        if is_container {
            let mut w = node.walk();
            for child in node.named_children(&mut w) {
                visit(&child, depth + 1, max_depth, out, visited);
            }
        }
    }
    visit(&root, 1, max_depth, out, &mut visited);
    let _ = path;
    Ok(())
}

fn read_file_range(path: &Path, start_line: u64, end_line: u64) -> Result<ToolResult, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("Failed to read `{}`: {e}", path.display()))?;
    let lines: Vec<&str> = text.lines().collect();
    let total = lines.len();
    let start = (start_line.max(1) as usize).saturating_sub(1).min(total);
    let end = (end_line as usize).min(total).max(start + 1);
    let mut out = String::new();
    for (i, line) in lines
        .iter()
        .enumerate()
        .skip(start)
        .take(end.saturating_sub(start))
    {
        out.push_str(&format!("{}: {line}\n", i + 1));
    }
    Ok(ToolResult::ok(
        "read_file_range",
        format!(
            "Read {} lines ({start_line}..={end_line}) of `{}`",
            end.saturating_sub(start),
            path.display()
        ),
        Some(format!("[{} lines total]\n{out}", total)),
        Some(json!({ "lines": total })),
    ))
}

fn read_file_chars(
    path: &Path,
    offset: Option<usize>,
    limit: Option<usize>,
) -> Result<ToolResult, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("Failed to read `{}`: {e}", path.display()))?;
    let offset = offset.unwrap_or(0).min(text.chars().count());
    let limit = limit.unwrap_or(4000);
    let slice: String = text.chars().skip(offset).take(limit).collect();
    Ok(ToolResult::ok(
        "read_file_chars",
        format!(
            "Read {} char(s) of `{}` from char {offset}",
            slice.chars().count(),
            path.display()
        ),
        Some(slice),
        Some(json!({ "totalChars": text.chars().count() })),
    ))
}

async fn write_file(
    app: &ToolContext,
    state: &ToolState,
    path: &Path,
    content: &str,
) -> Result<ToolResult, String> {
    let before = std::fs::read_to_string(path).ok();
    let root = state
        .primary_workspace()
        .await
        .or_else(|| path.parent().map(|p| p.to_path_buf()))
        .unwrap_or_default();
    let spec = FileEditSpec {
        path: path.to_string_lossy().into_owned(),
        line_start: None,
        line_end: None,
        patch: None,
        search: None,
        replace: None,
        full_content: Some(content.to_string()),
        reason: Some(format!("write_file: {} byte(s)", content.len())),
    };
    propose_edit(app, state, "write_file", &root, &spec, before).await
}

/// Propose a unified-diff edit for user review before touching the disk
/// (handles context-matched hunks, new files `@@ -0,0 +1,n @@`, and reports
/// exact mismatch locations so the model can self-correct).
async fn apply_diff_tool(
    app: &ToolContext,
    state: &ToolState,
    path: &Path,
    diff: &str,
) -> Result<ToolResult, String> {
    let content = if path.exists() {
        std::fs::read_to_string(path)
            .map_err(|e| format!("Failed to read `{}`: {e}", path.display()))?
    } else {
        String::new()
    };

    let root = state
        .primary_workspace()
        .await
        .or_else(|| path.parent().map(|p| p.to_path_buf()))
        .unwrap_or_default();
    let after_count = content.lines().count() + 1;
    let spec = FileEditSpec {
        path: path.to_string_lossy().into_owned(),
        line_start: None,
        line_end: None,
        patch: Some(diff.to_string()),
        search: None,
        replace: None,
        full_content: None,
        reason: Some(format!(
            "apply_file_diff on `{}` (≈{after_count} lines)",
            path.display()
        )),
    };
    propose_edit(app, state, "apply_file_diff", &root, &spec, Some(content)).await
}

/// Build a new file from a diff whose old side is empty/absent.
pub(crate) fn apply_new_file(diff: &str) -> Result<String, String> {
    let mut out = String::new();
    for line in diff.lines() {
        let l = line.trim_end();
        if l.starts_with("+++") || l.starts_with("---") || l.is_empty() {
            continue;
        }
        if l.starts_with("@@") {
            continue;
        }
        if l.starts_with('+') {
            out.push_str(l.strip_prefix('+').unwrap_or(l));
            out.push('\n');
        } else if l.starts_with('-') {
            // Removal lines in a new-file diff are just noise; skip.
        } else {
            out.push_str(l);
            out.push('\n');
        }
    }
    Ok(out)
}

/// Parse hunks of a standard unified diff and apply them to `original`.
pub(crate) fn apply_unified_diff(original: &str, diff: &str) -> Result<String, String> {
    let orig_lines: Vec<String> = original.lines().map(|l| l.to_string()).collect();

    let hunk_re = Regex::new(r"^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@(.*)$")
        .map_err(|e| e.to_string())?;

    fn parse_hunks(
        diff: &str,
        hunk_re: &Regex,
    ) -> Result<Vec<(usize, Vec<String>, Vec<String>)>, String> {
        let mut out = Vec::new();
        let mut cur: Option<(usize, Vec<String>, Vec<String>)> = None;
        for line in diff.lines() {
            if line.starts_with("+++") || line.starts_with("---") || line.is_empty() {
                continue;
            }
            if let Some(caps) = hunk_re.captures(line) {
                if let Some(h) = cur.take() {
                    out.push(h);
                }
                let old_start = caps.get(1).unwrap().as_str().parse().unwrap_or(1).max(1);
                cur = Some((old_start, Vec::new(), Vec::new()));
                continue;
            }
            if let Some((_, old, new)) = cur.as_mut() {
                if let Some(t) = line.strip_prefix('-') {
                    if !line.starts_with("---") {
                        old.push(t.to_string());
                    }
                } else if let Some(t) = line.strip_prefix('+') {
                    if !line.starts_with("+++") {
                        new.push(t.to_string());
                    }
                } else if let Some(t) = line.strip_prefix(' ') {
                    old.push(t.to_string());
                    new.push(t.to_string());
                }
            }
        }
        if let Some(h) = cur.take() {
            out.push(h);
        }
        Ok(out)
    }

    let parsed = parse_hunks(diff, &hunk_re)?;
    // Apply bottom-up so earlier line numbers stay valid.
    let mut result: Vec<String> = orig_lines.clone();
    for (i, (old_start, old, new)) in parsed.iter().enumerate().rev() {
        let idx = old_start - 1;
        // Validate (loosely) against the file content when in range.
        if idx < result.len() && !old.is_empty() {
            let mut mismatch = None;
            for (k, expected) in old.iter().enumerate() {
                if idx + k >= result.len() {
                    mismatch = Some(format!(
                        "hunk {} references line {} past EOF",
                        i + 1,
                        idx + k + 1
                    ));
                    break;
                }
                if result[idx + k].trim_end() != expected.trim_end() {
                    mismatch = Some(format!(
                        "hunk {} context mismatch at line {}: expected `{}`, found `{}`",
                        i + 1,
                        idx + k + 1,
                        expected,
                        result[idx + k]
                    ));
                    break;
                }
            }
            if let Some(m) = mismatch {
                return Err(m);
            }
        }
        result.splice(idx..idx.saturating_add(old.len()), new.clone());
    }
    Ok(result.join("\n"))
}

fn list_dir(path: &Path) -> Result<ToolResult, String> {
    if !path.is_dir() {
        return Err(format!("`{}` is not a directory", path.display()));
    }
    let mut entries: Vec<(bool, String)> = Vec::new();
    for e in
        std::fs::read_dir(path).map_err(|e| format!("Failed to list `{}`: {e}", path.display()))?
    {
        let e = e.map_err(|e| e.to_string())?;
        let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
        entries.push((is_dir, e.file_name().to_string_lossy().into_owned()));
    }
    entries.sort_by(|a, b| a.1.cmp(&b.1));
    let body = entries
        .iter()
        .map(|(dir, n)| if *dir { format!("{n}/") } else { n.clone() })
        .collect::<Vec<_>>()
        .join("\n");
    Ok(ToolResult::ok(
        "list_dir",
        format!("{} entr(y/ies) in `{}`", entries.len(), path.display()),
        Some(body),
        Some(json!({ "count": entries.len(), "dirs": entries.iter().filter(|(d, _)| *d).count() })),
    ))
}

fn create_folder(path: &Path) -> Result<ToolResult, String> {
    std::fs::create_dir_all(path)
        .map_err(|e| format!("Failed to create `{}`: {e}", path.display()))?;
    Ok(ToolResult::ok(
        "create_folder",
        format!("Created folder `{}`", path.display()),
        None,
        None,
    ))
}

fn copy_path(src: &Path, dst: &Path, can_overwrite: bool) -> Result<ToolResult, String> {
    if dst.exists() && !can_overwrite {
        return Err(format!(
            "Destination `{}` already exists (pass canOverwrite=true to replace)",
            dst.display()
        ));
    }
    if src.is_dir() {
        copy_dir_recursive(src, dst)?;
    } else {
        if let Some(p) = dst.parent() {
            std::fs::create_dir_all(p).map_err(|e| format!("mkdir {}: {e}", p.display()))?;
        }
        std::fs::copy(src, dst)
            .map_err(|e| format!("copy `{}` → `{}`: {e}", src.display(), dst.display()))?;
    }
    Ok(ToolResult::ok(
        "copy_file_or_folder",
        format!("Copied `{}` → `{}`", src.display(), dst.display()),
        None,
        None,
    ))
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<(), String> {
    if let Some(p) = dst.parent() {
        std::fs::create_dir_all(p).map_err(|e| e.to_string())?;
    }
    std::fs::create_dir_all(dst).map_err(|e| e.to_string())?;
    for e in std::fs::read_dir(src).map_err(|e| e.to_string())? {
        let e = e.map_err(|e| e.to_string())?;
        let t = e.file_type().map_err(|e| e.to_string())?;
        let from = e.path();
        let to = dst.join(e.file_name());
        if t.is_dir() {
            copy_dir_recursive(&from, &to)?;
        } else if t.is_file() || t.is_symlink() {
            std::fs::copy(&from, &to).map_err(|e| format!("copy {}: {e}", from.display()))?;
        }
    }
    Ok(())
}

fn move_path(src: &Path, dst: &Path, can_overwrite: bool) -> Result<ToolResult, String> {
    if dst.exists() && !can_overwrite {
        return Err(format!(
            "Destination `{}` already exists (pass canOverwrite=true to replace)",
            dst.display()
        ));
    }
    if let Some(p) = dst.parent() {
        std::fs::create_dir_all(p).map_err(|e| format!("mkdir {}: {e}", p.display()))?;
    }
    if can_overwrite && dst.exists() {
        std::fs::remove_file(dst)
            .or_else(|_| std::fs::remove_dir_all(dst))
            .map_err(|e| e.to_string())?;
    }
    std::fs::rename(src, dst)
        .map_err(|e| format!("move `{}` → `{}`: {e}", src.display(), dst.display()))?;
    Ok(ToolResult::ok(
        "move_file_or_folder",
        format!("Moved `{}` → `{}`", src.display(), dst.display()),
        None,
        None,
    ))
}

fn delete_path(path: &Path) -> Result<ToolResult, String> {
    if !path.exists() {
        return Err(format!("`{}` does not exist", path.display()));
    }
    trash::delete(path).map_err(|e| format!("Trash `{}` failed: {e}", path.display()))?;
    Ok(ToolResult::ok(
        "delete_file_or_folder",
        format!("Moved `{}` to the Trash", path.display()),
        None,
        None,
    ))
}

// ---------------------------------------------------------------------------
// Terminal / process tools
// ---------------------------------------------------------------------------

/// Shell-agnostic one-shot execution with a timeout + interrupt kill switch.
/// Returns `(exit_code, stdout, stderr)`.
async fn run_command_checked(
    program: &str,
    args: &[&str],
    cwd: Option<&Path>,
    timeout: Duration,
    interrupt: Option<&CancellationToken>,
) -> Result<(i32, String, String), String> {
    let mut cmd = tokio::process::Command::new(program);
    cmd.args(args);
    if let Some(c) = cwd {
        cmd.current_dir(c);
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("failed to spawn `{program}`: {e}"))?;

    async fn log_read(
        buf: &mut Vec<u8>,
        mut f: impl tokio::io::AsyncRead + Unpin,
    ) -> io::Result<()> {
        use tokio::io::AsyncReadExt;
        let mut tmp = [0u8; 4096];
        loop {
            let n = f.read(&mut tmp).await?;
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&tmp[..n]);
            if buf.len() > 32_000_000 {
                buf.truncate(32_000_000);
                break;
            }
        }
        Ok(())
    }

    let out_fut = child.stdout.take().map(|f| async {
        let mut buf = Vec::new();
        let _ = log_read(&mut buf, f).await;
        String::from_utf8_lossy(&buf).into_owned()
    });
    let err_fut = child.stderr.take().map(|f| async {
        let mut buf = Vec::new();
        let _ = log_read(&mut buf, f).await;
        String::from_utf8_lossy(&buf).into_owned()
    });
    if out_fut.is_none() || err_fut.is_none() {
        return Err(format!("`{program}` did not expose stdout/stderr"));
    }

    let wait = child.wait();
    let status: ExitStatus = if let Some(ik) = interrupt {
        tokio::select! {
            s = wait => s.map_err(|e| format!("wait error: {e}"))?,
            _ = ik.cancelled() => {
                let _ = child.kill().await;
                return Err("Execution Aborted".into());
            }
            _ = tokio::time::sleep(timeout) => {
                let _ = child.kill().await;
                return Err(format!("`{program}` timed out after {}s", timeout.as_secs()));
            }
        }
    } else {
        tokio::time::timeout(timeout, wait)
            .await
            .map_err(|_| format!("`{program}` timed out after {}s", timeout.as_secs()))?
            .map_err(|e| format!("wait error: {e}"))?
    };

    let out = out_fut.unwrap().await;
    let err = err_fut.unwrap().await;
    Ok((status.code().unwrap_or(-1), out, err))
}

#[cfg(windows)]
fn shell_command(command: &str) -> (&'static str, Vec<&'static str>, String) {
    ("cmd", vec!["/C"], command.to_string())
}

#[cfg(not(windows))]
fn shell_command(command: &str) -> (&'static str, Vec<&'static str>, String) {
    ("sh", vec!["-c"], command.to_string())
}

async fn execute_terminal_command(
    command: &str,
    cwd: Option<&Path>,
    timeout_secs: Option<u64>,
    interrupt: &CancellationToken,
) -> Result<ToolResult, String> {
    let (prog, mut args, script) = shell_command(command);
    args.push(Box::leak(script.into_boxed_str()));
    let timeout = Duration::from_secs(timeout_secs.unwrap_or(120));
    let started = Instant::now();
    match run_command_checked(prog, &args, cwd, timeout, Some(interrupt)).await {
        Ok((code, out, err)) => {
            let elapsed = started.elapsed().as_millis() as u64;
            let mut stdout = out.clone();
            if !err.trim().is_empty() {
                if !stdout.is_empty() {
                    stdout.push_str("\n");
                }
                stdout.push_str(&err);
            }
            if code == 0 {
                Ok(ToolResult::ok(
                    "execute_terminal_command",
                    format!("`{command}` exited 0 in {}ms", elapsed),
                    Some(truncate(&stdout, 80_000)),
                    Some(json!({ "exit_code": code, "duration_ms": elapsed })),
                ))
            } else {
                Ok(ToolResult::err(
                    "execute_terminal_command",
                    format!("`{command}` exited {code}"),
                    truncate(&stdout, 80_000),
                ))
            }
        }
        Err(e) => Err(e),
    }
}

async fn run_tests(
    ws: &Path,
    command: Option<&str>,
    interrupt: &CancellationToken,
) -> Result<ToolResult, String> {
    let resolved = match command {
        Some(c) if !c.trim().is_empty() => c.to_string(),
        _ => detect_test_command(ws)?,
    };
    let (prog, mut args, script) = shell_command(&resolved);
    args.push(Box::leak(script.into_boxed_str()));
    let timeout = Duration::from_secs(600);
    match run_command_checked(prog, &args, Some(ws), timeout, Some(interrupt)).await {
        Ok((code, out, err)) => {
            let mut stdout = out.clone();
            if !err.trim().is_empty() {
                if !stdout.is_empty() {
                    stdout.push_str("\n");
                }
                stdout.push_str(&err);
            }
            if code == 0 {
                Ok(ToolResult::ok(
                    "run_tests",
                    format!("`{resolved}` passed (exit 0)"),
                    Some(truncate(&stdout, 120_000)),
                    Some(json!({ "exit_code": 0 })),
                ))
            } else {
                Ok(ToolResult::err(
                    "run_tests",
                    format!("`{resolved}` failed (exit {code})"),
                    truncate(&stdout, 120_000),
                ))
            }
        }
        Err(e) => Err(e),
    }
}

fn detect_test_command(ws: &Path) -> Result<String, String> {
    if ws.join("Cargo.toml").is_file() {
        return Ok("cargo test".into());
    }
    if ws.join("package.json").is_file() {
        if let Ok(text) = std::fs::read_to_string(ws.join("package.json")) {
            if let Ok(v) = serde_json::from_str::<Value>(&text) {
                if v["scripts"]["test"].is_string() {
                    return Ok("npm test".into());
                }
            }
        }
    }
    if ws.join("pytest.ini").exists()
        || ws.join("pyproject.toml").exists()
        || ws.join("setup.py").is_file()
    {
        return Ok("python -m pytest".into());
    }
    Err("Cannot detect a test runner in this workspace. Specify a `command` explicitly (e.g. \"npm test\", \"cargo test\", \"pytest\").".into())
}

fn truncate(s: &str, cap: usize) -> String {
    let mut out: String = s.chars().take(cap).collect();
    if s.chars().count() > cap {
        out.push_str(&format!("\n… [output truncated at {cap} chars]"));
    }
    out
}

/// Run sandboxed interpreter code (python / node) in a scratchpad temp file.
async fn run_interp(
    ws: &Path,
    interp: &str,
    code: &str,
    timeout_secs: Option<u64>,
    interrupt: &CancellationToken,
) -> Result<ToolResult, String> {
    let bin = if interp == "python"
        && std::process::Command::new("python")
            .arg("--version")
            .output()
            .is_err()
    {
        "python3"
    } else {
        interp
    };
    let ext = if interp == "python" { "py" } else { "js" };
    let dir = super::scratchpad_root().join("runs");
    std::fs::create_dir_all(&dir).map_err(|e| format!("mkdir: {e}"))?;
    let file = dir.join(format!("run-{}-{}.{ext}", std::process::id(), now_ms()));
    std::fs::write(&file, code).map_err(|e| format!("write script: {e}"))?;
    let timeout = Duration::from_secs(timeout_secs.unwrap_or(60));
    let res = run_command_checked(
        bin,
        &[file.to_str().unwrap()],
        Some(ws),
        timeout,
        Some(interrupt),
    )
    .await;
    let _ = std::fs::remove_file(&file);
    match res {
        Ok((code, out, err)) => {
            let mut stdout = out.clone();
            if !err.trim().is_empty() {
                stdout.push_str("\n");
                stdout.push_str(&err);
            }
            let tool = if interp == "python" {
                "run_python"
            } else {
                "run_javascript"
            };
            if code == 0 {
                Ok(ToolResult::ok(
                    tool,
                    format!("`{bin}` exited 0"),
                    Some(truncate(&stdout, 60_000)),
                    Some(json!({ "exit_code": 0 })),
                ))
            } else {
                Ok(ToolResult::err(
                    tool,
                    format!("`{bin}` exited {code}"),
                    truncate(&stdout, 60_000),
                ))
            }
        }
        Err(e) => Err(e),
    }
}

// ---------------------------------------------------------------------------
// Calculate
// ---------------------------------------------------------------------------

/// Tiny deterministic arithmetic evaluator — no subprocess, no approval, so
/// small models can answer math reliably. Supports + - * / %, ^, parens,
/// unary minus, `pi`/`e`, and one-arg functions.
fn calculate(expression: &str) -> Result<ToolResult, String> {
    let value = eval_expr(expression)?;
    let pretty = if value.fract().abs() < 1e-9 && value.abs() < 1e15 {
        format!("{:.0}", value)
    } else {
        format!("{}", value)
    };
    Ok(ToolResult::ok(
        "calculate",
        format!("`{expression}` = {pretty}"),
        Some(pretty),
        Some(json!({ "result": value })),
    ))
}

fn replace_constants(s: &str) -> String {
    let mut out = s.to_string();
    if let Some(i) = out.find("pi") {
        let mut r = out.to_string();
        r.replace_range(i..i + 2, "3.141592653589793");
        out = r;
    }
    let mut idx = 0;
    while idx + 1 <= out.len() {
        if &out[idx..(idx + 1).min(out.len())] == "e" {
            let before = idx
                .checked_sub(1)
                .and_then(|b| out.as_bytes().get(b))
                .copied();
            let after = out.as_bytes().get(idx + 1).copied();
            let prev_digit = before
                .map(|b| b.is_ascii_digit() || b == b'.' || b == b'_')
                .unwrap_or(false);
            let next_digit = after
                .map(|b| b.is_ascii_digit() || b == b'.')
                .unwrap_or(false);
            if !prev_digit && !next_digit {
                out.replace_range(idx..idx + 1, "2.718281828459045");
                idx += 16;
                continue;
            }
        }
        idx += 1;
    }
    out
}

fn eval_expr(src: &str) -> Result<f64, String> {
    let scrubbed = replace_constants(src);
    let tokens = expr_tokenize(&scrubbed)?;
    let rpn = shunting_yard(tokens)?;
    eval_rpn(rpn)
}

#[derive(Debug, Clone)]
enum Tok {
    Num(f64),
    Op(char),
    Fun(String),
    LParen,
    RParen,
    Comma,
}

fn expr_tokenize(s: &str) -> Result<Vec<Tok>, String> {
    let mut out = Vec::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i] as char;
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        if c.is_ascii_digit() || c == '.' {
            let start = i;
            let mut saw_dot = false;
            while i < bytes.len() {
                let ch = bytes[i] as char;
                if ch.is_ascii_digit() {
                    i += 1;
                } else if ch == '.' && !saw_dot {
                    saw_dot = true;
                    i += 1;
                } else if (ch == 'e' || ch == 'E')
                    && i + 1 < bytes.len()
                    && (bytes[i + 1] as char == '-'
                        || bytes[i + 1] as char == '+'
                        || (bytes[i + 1] as char).is_ascii_digit())
                {
                    i += 2;
                    while i < bytes.len() && (bytes[i] as char).is_ascii_digit() {
                        i += 1;
                    }
                } else {
                    break;
                }
            }
            let num = s[start..i]
                .parse::<f64>()
                .map_err(|e| format!("Bad number `{}`: {e}", &s[start..i]))?;
            out.push(Tok::Num(num));
            continue;
        }
        match c {
            '+' | '-' | '*' | '/' | '^' | '%' => {
                out.push(Tok::Op(c));
                i += 1;
            }
            '(' => {
                out.push(Tok::LParen);
                i += 1;
            }
            ')' => {
                out.push(Tok::RParen);
                i += 1;
            }
            ',' => {
                out.push(Tok::Comma);
                i += 1;
            }
            c if c.is_alphabetic() => {
                let start = i;
                while i < bytes.len() && (bytes[i] as char).is_alphanumeric() {
                    i += 1;
                }
                let name = s[start..i].to_string();
                match name.as_str() {
                    "sqrt" | "abs" | "round" | "floor" | "ceil" | "ln" | "log10" | "sin"
                    | "cos" | "tan" | "exp" => out.push(Tok::Fun(name)),
                    _ => return Err(format!("Unknown function/constant `{name}`")),
                }
            }
            other => return Err(format!("Unexpected character `{other}`")),
        }
    }
    Ok(out)
}

fn shunting_yard(tokens: Vec<Tok>) -> Result<Vec<Tok>, String> {
    let mut out: Vec<Tok> = Vec::new();
    let mut ops: Vec<Tok> = Vec::new();
    let mut prev: Option<Tok> = None;
    for tok in tokens {
        let prev_tok = tok.clone();
        match tok {
            Tok::Num(_) => {
                out.push(tok);
            }
            Tok::Fun(_) => ops.push(tok),
            Tok::LParen => ops.push(tok),
            Tok::RParen => loop {
                match ops.pop() {
                    Some(Tok::LParen) => break,
                    Some(o) => out.push(o),
                    None => return Err("Mismatched parentheses".into()),
                }
            },
            Tok::Comma => loop {
                match ops.last() {
                    Some(Tok::LParen) | None => break,
                    _ => out.push(ops.pop().unwrap()),
                }
            },
            Tok::Op(c) => {
                let unary = matches!(
                    &prev,
                    None | Some(Tok::Op(_)) | Some(Tok::LParen) | Some(Tok::Comma)
                ) && (c == '-');
                if unary {
                    ops.push(Tok::Op('~')); // unary minus
                } else {
                    let prec = |o: char| match o {
                        '^' => 4,
                        '*' | '/' | '%' => 3,
                        '+' | '-' => 2,
                        _ => 0,
                    };
                    while let Some(Tok::Op(o)) = ops.last() {
                        let p_o = prec(*o);
                        let pop = if c == '^' {
                            p_o > prec(c)
                        } else {
                            p_o >= prec(c)
                        };
                        if !pop {
                            break;
                        }
                        out.push(ops.pop().unwrap());
                    }
                    ops.push(Tok::Op(c));
                }
            }
        }
        prev = Some(prev_tok);
    }
    while let Some(o) = ops.pop() {
        if matches!(o, Tok::LParen) {
            return Err("Mismatched parentheses".into());
        }
        out.push(o);
    }
    Ok(out)
}

fn eval_rpn(rpn: Vec<Tok>) -> Result<f64, String> {
    let mut stack: Vec<f64> = Vec::new();
    for t in rpn {
        match t {
            Tok::Num(n) => stack.push(n),
            Tok::Op('~') => {
                let a = stack.pop().ok_or("empty stack")?;
                stack.push(-a);
            }
            Tok::Op(o) => {
                let b = stack.pop().ok_or_else(|| "empty stack".to_string())?;
                let a = stack.pop().ok_or_else(|| "empty stack".to_string())?;
                let r = match o {
                    '+' => a + b,
                    '-' => a - b,
                    '*' => a * b,
                    '/' => {
                        if b.abs() < f64::EPSILON {
                            return Err("Division by zero".into());
                        }
                        a / b
                    }
                    '%' => {
                        if b.abs() < f64::EPSILON {
                            return Err("Modulo by zero".into());
                        }
                        a % b
                    }
                    '^' => a.powf(b),
                    _ => return Err(format!("Unknown operator `{o}`")),
                };
                stack.push(r);
            }
            Tok::Fun(f) => {
                let a = stack.pop().ok_or("empty stack")?;
                let r = match f.as_str() {
                    "sqrt" => a.sqrt(),
                    "abs" => a.abs(),
                    "round" => a.round(),
                    "floor" => a.floor(),
                    "ceil" => a.ceil(),
                    "ln" => a.ln(),
                    "log10" => a.log10(),
                    "sin" => a.sin(),
                    "cos" => a.cos(),
                    "tan" => a.tan(),
                    "exp" => a.exp(),
                    _ => return Err(format!("Unknown function `{f}`")),
                };
                stack.push(r);
            }
            Tok::LParen | Tok::RParen | Tok::Comma => return Err("Invalid expression".into()),
        }
    }
    stack.pop().ok_or("Empty expression".into())
}

// ---------------------------------------------------------------------------
// Git tools
// ---------------------------------------------------------------------------

async fn run_git(
    ws: &Option<&Path>,
    args: &[String],
    timeout: Duration,
) -> Result<(i32, String, String), String> {
    let ws_real: Option<&Path> = *ws;
    let cwd = ws_real;
    let (code, out, err) = run_command_checked_owned("git", args, cwd, timeout, None).await?;
    let _ = cwd;
    Ok((code, out, err))
}

async fn run_command_checked_owned(
    program: &str,
    args: &[String],
    cwd: Option<&Path>,
    timeout: Duration,
    interrupt: Option<&CancellationToken>,
) -> Result<(i32, String, String), String> {
    let refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    run_command_checked(program, &refs, cwd, timeout, interrupt).await
}

fn first_ws(ws: &[PathBuf]) -> Option<&Path> {
    ws.first().map(|p| p.as_path())
}

async fn git_status(ws: &[PathBuf]) -> Result<ToolResult, String> {
    let root = first_ws(ws).ok_or_else(|| "No workspace set".to_string())?;
    let (code, out, err) = run_git(
        &Some(root),
        &["status".into(), "--porcelain=v1".into(), "--branch".into()],
        Duration::from_secs(60),
    )
    .await?;
    if code != 0 {
        return Err(format!("git status failed: {err}"));
    }
    Ok(ToolResult::ok(
        "git_status",
        "Workspace git status".to_string(),
        Some(truncate(&out, 40_000)),
        Some(json!({ "exit_code": code })),
    ))
}

async fn git_diff(ws: &[PathBuf], path: Option<&str>) -> Result<ToolResult, String> {
    let root = first_ws(ws).ok_or_else(|| "No workspace set".to_string())?;
    let mut args = vec!["diff".to_string(), "--stat".to_string()];
    if let Some(p) = path {
        args.push("--".to_string());
        args.push(p.to_string());
    }
    let (code0, stat, err0) = run_git(&Some(root), &args, Duration::from_secs(60)).await?;
    let full_args = match path {
        Some(p) => vec!["diff".into(), "--".into(), p.to_string()],
        None => vec!["diff".into()],
    };
    let (code1, full, err1) = run_git(&Some(root), &full_args, Duration::from_secs(60)).await?;
    if code0 != 0 || code1 != 0 {
        return Err(format!("git diff failed: {err0}{err1}"));
    }
    let body = format!("{stat}\n\n{full}");
    Ok(ToolResult::ok(
        "git_diff",
        "Workspace diff".to_string(),
        Some(truncate(&body, 80_000)),
        None,
    ))
}

async fn git_commit(ws: &[PathBuf], message: &str) -> Result<ToolResult, String> {
    let root = first_ws(ws).ok_or_else(|| "No workspace set".to_string())?;
    let (addc, _, adderr) = run_git(
        &Some(root),
        &["add".into(), "-A".into()],
        Duration::from_secs(60),
    )
    .await?;
    if addc != 0 {
        return Err(format!("git add failed: {adderr}"));
    }
    let (c, out, err) = run_git(
        &Some(root),
        &["commit".into(), "-m".into(), message.to_string()],
        Duration::from_secs(60),
    )
    .await?;
    if c != 0 {
        return Ok(ToolResult::err(
            "git_commit",
            "Commit failed".to_string(),
            truncate(&err, 4000),
        ));
    }
    Ok(ToolResult::ok(
        "git_commit",
        "Committed changes".to_string(),
        Some(truncate(&out, 4000)),
        None,
    ))
}

/// Create a checkpoint commit (`git add -A` + `git commit`). Additive and
/// safe; used both by the tool and the one-click UI command.
pub async fn git_checkpoint(
    state: &ToolState,
    interrupt: &CancellationToken,
    message: Option<&str>,
) -> Result<ToolResult, String> {
    let ws = state
        .primary_workspace()
        .await
        .ok_or_else(|| "No workspace set".to_string())?;
    let msg = message.unwrap_or("ai: checkpoint").to_string();
    let args: Vec<String> = vec!["add".into(), "-A".into()];
    let (ac, _, aerr) = run_command_checked_owned(
        "git",
        &args,
        Some(&ws),
        Duration::from_secs(120),
        Some(interrupt),
    )
    .await?;
    if ac != 0 {
        return Err(format!("git add failed: {aerr}"));
    }
    let args: Vec<String> = vec!["commit".into(), "-m".into(), msg];
    let (c, out, err) = run_command_checked_owned(
        "git",
        &args,
        Some(&ws),
        Duration::from_secs(120),
        Some(interrupt),
    )
    .await?;
    if c != 0 {
        return Ok(ToolResult::err(
            "git_checkpoint",
            "Checkpoint commit failed".to_string(),
            truncate(&err, 4000),
        ));
    }
    Ok(ToolResult::ok(
        "git_checkpoint",
        "Created a git checkpoint".to_string(),
        Some(truncate(&out, 4000)),
        None,
    ))
}

/// List recent commits as `{hash, subject, relative}` values for the
/// one-click revert UI.
pub async fn git_checkpoints(
    state: &ToolState,
    interrupt: &CancellationToken,
) -> Result<Vec<Value>, String> {
    let ws = state
        .primary_workspace()
        .await
        .ok_or_else(|| "No workspace set".to_string())?;
    let args: Vec<String> = vec![
        "log".into(),
        "-20".into(),
        "--pretty=format:%H%x1f%s%x1f%cr".into(),
    ];
    let (c, out, err) = run_command_checked_owned(
        "git",
        &args,
        Some(&ws),
        Duration::from_secs(60),
        Some(interrupt),
    )
    .await?;
    if c != 0 {
        return Err(format!("git log failed: {err}"));
    }
    let mut values = Vec::new();
    for line in out.lines() {
        let mut parts = line.splitn(3, '\u{1f}');
        let hash = parts.next().unwrap_or("").to_string();
        let subject = parts.next().unwrap_or("").to_string();
        let relative = parts.next().unwrap_or("").to_string();
        values.push(json!({ "hash": hash, "subject": subject, "relative": relative }));
    }
    Ok(values)
}

/// Hard-reset to a checkpoint commit.
pub async fn git_revert(
    state: &ToolState,
    interrupt: &CancellationToken,
    commit: Option<&str>,
) -> Result<ToolResult, String> {
    let ws = state
        .primary_workspace()
        .await
        .ok_or_else(|| "No workspace set".to_string())?;
    let hash = commit.unwrap_or("HEAD");
    let args: Vec<String> = vec!["reset".into(), "--hard".into(), hash.to_string()];
    let (c, out, err) = run_command_checked_owned(
        "git",
        &args,
        Some(&ws),
        Duration::from_secs(120),
        Some(interrupt),
    )
    .await?;
    if c != 0 {
        return Ok(ToolResult::err(
            "git_revert",
            "Revert failed".to_string(),
            truncate(&err, 4000),
        ));
    }
    Ok(ToolResult::ok(
        "git_revert",
        format!("Reset to `{hash}`"),
        Some(truncate(&out, 4000)),
        None,
    ))
}

async fn git_blame(
    ws: &[PathBuf],
    path: &Path,
    start: Option<u64>,
    end: Option<u64>,
) -> Result<ToolResult, String> {
    let root = first_ws(ws).ok_or_else(|| "No workspace set".to_string())?;
    let rel = path
        .strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/");
    let mut args = vec!["blame".to_string()];
    if let (Some(s), Some(e)) = (start, end) {
        args.push(format!("-L {s},{e}"));
    }
    args.push("--".to_string());
    args.push(rel.clone());
    let (c, out, err) = run_git(&Some(root), &args, Duration::from_secs(120)).await?;
    if c != 0 {
        return Err(format!("git blame: {err}"));
    }
    Ok(ToolResult::ok(
        "git_blame",
        format!("Blame for {rel}"),
        Some(truncate(&out, 40_000)),
        None,
    ))
}

async fn git_push(
    ws: &[PathBuf],
    remote: Option<&str>,
    branch: Option<&str>,
    set_upstream: bool,
) -> Result<ToolResult, String> {
    let root = first_ws(ws).ok_or_else(|| "No workspace set".to_string())?;
    let mut args = vec!["push".to_string()];
    if set_upstream {
        args.push("-u".to_string());
    }
    if let Some(r) = remote {
        args.push(r.to_string());
    }
    if let Some(b) = branch {
        args.push(b.to_string());
    }
    let (c, out, err) = run_git(&Some(root), &args, Duration::from_secs(300)).await?;
    if c != 0 {
        return Err(format!("git push failed: {err}"));
    }
    Ok(ToolResult::ok(
        "git_push",
        "Pushed to remote".to_string(),
        Some(truncate(&out, 4000)),
        None,
    ))
}

async fn git_pull(ws: &[PathBuf]) -> Result<ToolResult, String> {
    let root = first_ws(ws).ok_or_else(|| "No workspace set".to_string())?;
    let (c, out, err) = run_git(&Some(root), &["pull".into()], Duration::from_secs(300)).await?;
    if c != 0 {
        return Err(format!("git pull failed: {err}"));
    }
    Ok(ToolResult::ok(
        "git_pull",
        "Pulled from remote".to_string(),
        Some(truncate(&out, 4000)),
        None,
    ))
}

async fn git_create_branch(ws: &[PathBuf], name: &str) -> Result<ToolResult, String> {
    let root = first_ws(ws).ok_or_else(|| "No workspace set".to_string())?;
    let (ec, _, _) = run_git(
        &Some(root),
        &[
            "rev-parse".into(),
            "--verify".into(),
            format!("refs/heads/{name}"),
        ],
        Duration::from_secs(30),
    )
    .await?;
    if ec == 0 {
        return Err(format!("Branch `{name}` already exists"));
    }
    let (c, out, err) = run_git(
        &Some(root),
        &["switch".into(), "-c".into(), name.to_string()],
        Duration::from_secs(60),
    )
    .await?;
    if c != 0 {
        return Err(format!("git switch -c `{name}` failed: {err}"));
    }
    Ok(ToolResult::ok(
        "git_create_branch",
        format!("Created and switched to `{name}`"),
        Some(truncate(&out, 4000)),
        None,
    ))
}

async fn run_gh(ws: &[PathBuf], args: &[String]) -> Result<(i32, String, String), String> {
    let root = first_ws(ws).ok_or_else(|| "No workspace set".to_string())?;
    // `gh`, not `git`: this backs the GitHub tools (`gh pr status`, `gh run
    // list`, `gh pr create`). Running them through `git` made every call fail
    // with an unknown-subcommand error.
    run_command_checked_owned("gh", args, Some(root), Duration::from_secs(120), None).await
}

async fn git_pr_status(ws: &[PathBuf]) -> Result<ToolResult, String> {
    let (c, out, err) = run_gh(ws, &["pr".to_string(), "status".to_string()]).await?;
    if c != 0 && err.contains("gh") {
        return Err(format!("`gh` CLI not available: {err}"));
    }
    Ok(ToolResult::ok(
        "git_pr_status",
        "Pull-request status".to_string(),
        Some(truncate(&if c == 0 { out } else { err }, 30_000)),
        None,
    ))
}

async fn git_ci_status(ws: &[PathBuf]) -> Result<ToolResult, String> {
    let (c, out, err) = run_gh(
        ws,
        &[
            "run".to_string(),
            "list".to_string(),
            "--limit".to_string(),
            "8".to_string(),
        ],
    )
    .await?;
    if c != 0 {
        return Err(format!("`gh run list` failed (is gh installed?): {err}"));
    }
    Ok(ToolResult::ok(
        "git_ci_status",
        "Recent CI runs".to_string(),
        Some(truncate(&out, 30_000)),
        None,
    ))
}

async fn create_pr(ws: &[PathBuf], title: &str, body: Option<&str>) -> Result<ToolResult, String> {
    let mut args = vec![
        "pr".to_string(),
        "create".to_string(),
        "--title".to_string(),
        title.to_string(),
    ];
    if let Some(b) = body {
        args.push("--body".to_string());
        args.push(b.to_string());
    }
    let (c, out, err) = run_gh(ws, &args).await?;
    if c != 0 {
        return Err(format!("`gh pr create` failed: {err}"));
    }
    Ok(ToolResult::ok(
        "create_pr",
        format!("Opened PR: {title}"),
        Some(truncate(&out, 4000)),
        None,
    ))
}

// ---------------------------------------------------------------------------
// Skills tools
// ---------------------------------------------------------------------------

fn create_skill(
    state: &ToolState,
    name: &str,
    description: Option<&str>,
    content: &str,
) -> Result<ToolResult, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("Skill name cannot be empty".into());
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err("Skill names may only contain letters, digits, `-` and `_`".into());
    }
    let ws = state
        .workspace
        .blocking_lock()
        .first()
        .cloned()
        .ok_or_else(|| "No workspace set".to_string())?;
    let dir = ws.join(".ai").join("skills");
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("Failed to create {}: {e}", dir.display()))?;
    let file = dir.join(format!("{name}.md"));
    if file.exists() {
        return Err(format!(
            "Skill `{name}` already exists at {}",
            file.display()
        ));
    }
    let desc = description.unwrap_or("").trim();
    let md = format!("---\nname: {name}\ndescription: {desc}\n---\n\n{content}\n");
    std::fs::write(&file, md).map_err(|e| format!("Failed to write `{}`: {e}", file.display()))?;

    // Reflect immediately into the in-memory knowledge state.
    {
        let mut map = state.knowledge.skills.lock().unwrap();
        map.insert(
            name.to_string(),
            skills::Skill {
                name: name.to_string(),
                description: desc.to_string(),
                content: content.to_string(),
                source: file.display().to_string(),
                active: true,
                tags: Vec::new(),
                globs: Vec::new(),
                abs_path: file.clone(),
            },
        );
    }
    Ok(ToolResult::ok(
        "create_skill",
        format!("Learned skill `{name}`"),
        Some(format!("Skill `{name}` written to {}", file.display())),
        None,
    ))
}

fn read_skill(state: &ToolState, name: &str) -> Result<ToolResult, String> {
    let skill = {
        let skills = state.knowledge.skills.lock().unwrap();
        skills.get(name).cloned()
    };
    match skill {
        Some(s) => Ok(ToolResult::ok(
            "read_skill",
            format!("Skill `{name}`"),
            Some(format!(
                "# {}\n\n(description: {})\n\n{}",
                s.name, s.description, s.content
            )),
            None,
        )),
        None => Err(format!("No skill named `{name}`")),
    }
}

fn suggest_skills(
    state: &ToolState,
    prompt: &str,
    path: Option<&str>,
) -> Result<ToolResult, String> {
    let suggestions = state.knowledge.suggest(prompt, path);
    if suggestions.is_empty() {
        return Ok(ToolResult::ok(
            "suggest_skills",
            "No matching skills".into(),
            Some("No existing skills match this task; consider `create_skill`.".into()),
            None,
        ));
    }
    let mut body = String::new();
    for (score, s) in &suggestions {
        body.push_str(&format!(
            "- **{}** (score {}): {}\n",
            s.name, score, s.description
        ));
    }
    Ok(ToolResult::ok(
        "suggest_skills",
        format!("{} relevant skill(s)", suggestions.len()),
        Some(body),
        Some(json!({ "count": suggestions.len() })),
    ))
}

// ---------------------------------------------------------------------------
// MCP tools
// ---------------------------------------------------------------------------

fn app_config_dir(app: &ToolContext) -> PathBuf {
    app.config_dir.clone()
}

async fn call_mcp_tool(
    app: &ToolContext,
    state: &ToolState,
    workspaces: &[PathBuf],
    tool: &str,
    arguments: &Value,
    timeout_secs: Option<u64>,
    server: &Option<String>,
    server_bin: &Option<String>,
    server_args: &[String],
) -> Result<ToolResult, String> {
    let config_dir = app_config_dir(app);

    // Resolve the server config: catalog match by name first, else ad-hoc stdio.
    let servers = mcp::load_catalog(&config_dir).unwrap_or_default();
    let cfg = if let Some(name) = server {
        servers
            .iter()
            .find(|c| c.name == *name)
            .cloned()
            .ok_or_else(|| format!("MCP server `{name}` not configured"))?
    } else if let Some(b) = server_bin {
        mcp::McpServerConfig {
            name: b.clone(),
            bin: b.clone(),
            args: server_args.to_vec(),
            ..Default::default()
        }
    } else {
        return Err(
            "call_mcp_tool requires either a configured `server` name or `serverBin`".to_string(),
        );
    };

    // Honour per-server enable + per-tool allow-list for catalogued servers.
    // Ad-hoc `serverBin` calls (no catalog entry) stay allowed for back-compat.
    if !cfg.enabled {
        return Err(format!(
            "MCP server `{}` is disabled in Settings — enable it to call its tools",
            cfg.name
        ));
    }
    if server.is_some() && !cfg.tool_allowed(tool) {
        return Err(format!(
            "MCP tool `{tool}` is not enabled for server `{}` — adjust its allowed tools in Settings",
            cfg.name
        ));
    }

    let key = cfg.cache_key();
    let handle = {
        let mut cache = state.mcp_servers.lock().await;
        if let Some(h) = cache.get(&key) {
            h.clone()
        } else {
            let h = mcp::McpClient::connect(&cfg)
                .await
                .map_err(|e| format!("Failed to start MCP server `{}`: {e}", cfg.name))?;
            let arc = std::sync::Arc::new(tokio::sync::Mutex::new(h));
            cache.insert(key.clone(), arc.clone());
            arc
        }
    };

    let timeout = Duration::from_secs(timeout_secs.unwrap_or(120));
    let mut guard = handle.lock().await;
    let result = tokio::time::timeout(timeout, guard.call_tool(tool, arguments))
        .await
        .map_err(|_| format!("MCP tool `{tool}` timed out after {}s", timeout.as_secs()))?
        .map_err(|e| format!("MCP tool `{tool}` failed: {e}"))?;
    drop(guard);

    let text = result
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let _ = workspaces;
    Ok(ToolResult::ok(
        "call_mcp_tool",
        format!("MCP `{tool}` returned"),
        Some(truncate(&text, 60_000)),
        Some(json!({ "server": server, "tool": tool })),
    ))
}

fn list_mcp_servers(app: &ToolContext) -> Result<ToolResult, String> {
    let servers = mcp::load_catalog(&app_config_dir(app)).unwrap_or_default();
    let body: Vec<Value> = servers
        .iter()
        .map(|s| {
            json!({
                "name": s.name,
                "transport": s.transport,
                "bin": s.bin,
                "args": s.args,
                "url": s.url,
                "enabled": s.enabled,
                "allowed_tools": s.allowed_tools,
            })
        })
        .collect();
    Ok(ToolResult::ok(
        "list_mcp_servers",
        format!("{} MCP server(s)", servers.len()),
        Some(serde_json::to_string_pretty(&body).unwrap_or_default()),
        Some(json!({ "count": servers.len() })),
    ))
}

async fn add_mcp_server(
    app: &ToolContext,
    name: &str,
    bin: &str,
    args: &[String],
) -> Result<Vec<mcp::McpServerConfig>, String> {
    let config_dir = app_config_dir(app);
    let mut servers = mcp::load_catalog(&config_dir).unwrap_or_default();
    if servers.iter().any(|s| s.name == name) {
        return Err(format!("MCP server `{name}` already configured"));
    }
    servers.push(mcp::McpServerConfig {
        name: name.to_string(),
        bin: bin.to_string(),
        args: args.to_vec(),
        enabled: true,
        ..Default::default()
    });
    mcp::save_catalog(&config_dir, &servers).map_err(|e| e.to_string())?;
    Ok(servers)
}

async fn remove_mcp_server(app: &ToolContext, name: &str) -> Result<ToolResult, String> {
    let config_dir = app_config_dir(app);
    let mut servers = mcp::load_catalog(&config_dir).unwrap_or_default();
    let before = servers.len();
    servers.retain(|s| s.name != name);
    if servers.len() == before {
        return Err(format!("No MCP server named `{name}`"));
    }
    mcp::save_catalog(&config_dir, &servers).map_err(|e| e.to_string())?;
    Ok(ToolResult::ok(
        "remove_mcp_server",
        format!("Removed MCP server `{name}`"),
        None,
        None,
    ))
}

// ---------------------------------------------------------------------------
// Attachments (RAG)
// ---------------------------------------------------------------------------

fn attach_file(state: &ToolState, path: &Path) -> Result<rag::AttachedFile, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("Failed to read `{}`: {e}", path.display()))?;
    if text.len() > 10_000_000 {
        return Err(format!("`{}` is too large to index", path.display()));
    }
    state
        .rag
        .lock()
        .unwrap()
        .attach(&path.to_string_lossy(), &text)
}

fn search_attached(state: &ToolState, query: &str, top_k: usize) -> Result<ToolResult, String> {
    let hits = state.rag.lock().unwrap().search(query, top_k);
    if hits.is_empty() {
        return Ok(ToolResult::ok(
            "search_attached_files",
            "No matching attachments".into(),
            Some("No chunks matched the query.".into()),
            None,
        ));
    }
    let mut body = String::new();
    for (file, offset, score, text) in &hits {
        body.push_str(&format!(
            "{file} (offset {offset}, score {score:.3}):\n{text}\n\n"
        ));
    }
    Ok(ToolResult::ok(
        "search_attached_files",
        format!("{} chunk(s) matched", hits.len()),
        Some(truncate(&body, 60_000)),
        Some(json!({ "count": hits.len() })),
    ))
}

// ---------------------------------------------------------------------------
// Transcription
// ---------------------------------------------------------------------------

/// Transcribe an audio file with a compatible whisper.cpp CLI. The engine is
/// located via `AI_EDITOR_WHISPER_BIN` or on PATH (`whisper-cli` /
/// `whisper-cpp` / `ai-whisper`). A model file must also be configured via
/// `AI_EDITOR_WHISPER_MODEL` (or a stock `ggml-base.bin` next to the binary).
pub async fn transcribe_file(state: &ToolState, path: &Path) -> Result<String, String> {
    let _ = state;
    if !path.exists() {
        return Err(format!("`{}` does not exist", path.display()));
    }
    let bin = std::env::var("AI_EDITOR_WHISPER_BIN")
        .ok()
        .filter(|b| !b.is_empty())
        .or_else(find_whisper_bin)
        .ok_or_else(|| {
            String::from("No transcription engine found. Set AI_EDITOR_WHISPER_BIN to a whisper.cpp compatible CLI.")
        })?;
    let model = std::env::var("AI_EDITOR_WHISPER_MODEL")
        .ok()
        .filter(|m| !m.is_empty())
        .ok_or_else(|| {
            String::from(
                "Set AI_EDITOR_WHISPER_MODEL to a whisper.cpp ggml model (e.g. ggml-base.bin).",
            )
        })?;

    let dir = super::scratchpad_root().join("transcribe");
    std::fs::create_dir_all(&dir).map_err(|e| format!("mkdir: {e}"))?;
    let base = dir.join(format!(
        "w-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0)
    ));

    let audio = path.to_string_lossy().into_owned();
    let out_base = base.to_string_lossy().into_owned();
    let (code, out, err) = run_command_checked_owned(
        &bin,
        &vec![
            "-m".to_string(),
            model,
            "-f".to_string(),
            audio,
            "-oj".to_string(),
            "-of".to_string(),
            out_base.clone(),
            "-nt".to_string(),
        ],
        None,
        Duration::from_secs(600),
        None,
    )
    .await?;
    if code != 0 {
        return Err(format!("Whisper failed (exit {code}): {err}"));
    }
    let json_path = format!("{out_base}.json");
    let text =
        std::fs::read_to_string(&json_path).map_err(|e| format!("No transcript produced: {e}"))?;
    let _ = std::fs::remove_file(&json_path);
    let mut transcript = String::new();
    if let Ok(v) = serde_json::from_str::<Value>(&text) {
        if let Some(segs) = v["transcription"].as_array() {
            for s in segs {
                if let Some(t) = s["text"].as_str() {
                    transcript.push_str(t);
                }
            }
        } else if let Some(segs) = v["segments"].as_array() {
            for s in segs {
                if let Some(t) = s["text"].as_str() {
                    transcript.push_str(t);
                }
            }
        }
    }
    let _ = out;
    Ok(transcript)
}

fn find_whisper_bin() -> Option<String> {
    let path = std::env::var_os("PATH")?;
    let names = ["whisper-cli", "whisper-cpp", "ai-whisper", "whisper"];
    for dir in std::env::split_paths(&path) {
        for name in names {
            for full in [dir.join(name), dir.join(format!("{name}.exe"))] {
                if full.is_file() {
                    return Some(full.to_string_lossy().into_owned());
                }
            }
        }
    }
    None
}

async fn transcribe_audio_tool(path: &Path, _language: Option<&str>) -> Result<ToolResult, String> {
    // `transcribe_file` already falls back to a solid engine message.
    let mut out = transcribe_file(&ToolState::default(), path)
        .await
        .map_err(|e| e)?;
    if out.is_empty() {
        out = "(no speech detected)".to_string();
    }
    Ok(ToolResult::ok(
        "transcribe_audio",
        "Transcription complete".into(),
        Some(out),
        None,
    ))
}

// ---------------------------------------------------------------------------
// Plan tools
// ---------------------------------------------------------------------------

fn create_plan(
    state: &ToolState,
    workspace: Option<&Path>,
    title: &str,
    goal: &str,
    items: &[String],
) -> Result<ToolResult, String> {
    let ws = workspace.ok_or_else(|| "No workspace set".to_string())?;
    let plan = plan::new_plan(title, goal, items.to_vec());
    plan.save(ws)?;
    *state.plan.lock().unwrap() = Some(plan.clone());
    Ok(ToolResult::ok(
        "create_plan",
        format!("Created plan `{title}` ({} item(s))", plan.items.len()),
        Some(plan.render_markdown()),
        None,
    ))
}

fn read_plan(state: &ToolState, workspace: Option<&Path>) -> Result<ToolResult, String> {
    let ws = workspace.ok_or_else(|| "No workspace set".to_string())?;
    let plan = state
        .plan
        .lock()
        .unwrap()
        .clone()
        .or_else(|| plan::PlanState::load(ws));
    match plan {
        Some(p) => Ok(ToolResult::ok(
            "read_plan",
            format!("Plan `{}`", p.title),
            Some(p.render_markdown()),
            None,
        )),
        None => Err("No active plan. Use `create_plan` first.".into()),
    }
}

fn update_plan(
    state: &ToolState,
    workspace: Option<&Path>,
    item: usize,
    status: &str,
    details: Option<&str>,
) -> Result<ToolResult, String> {
    let ws = workspace.ok_or_else(|| "No workspace set".to_string())?;
    let mut plan = state
        .plan
        .lock()
        .unwrap()
        .clone()
        .or_else(|| plan::PlanState::load(ws))
        .ok_or_else(|| "No active plan".to_string())?;
    if item < 1 || item > plan.items.len() {
        return Err(format!(
            "Plan item #{item} is out of range (1..={})",
            plan.items.len()
        ));
    }
    let pstatus = plan::PlanStatus::from_label(status)
        .ok_or_else(|| format!("Unknown plan status `{status}`"))?;
    plan.items[item - 1].status = pstatus;
    if let Some(d) = details {
        plan.items[item - 1].details = d.to_string();
    }
    plan.updated_at = now_ms();
    plan.save(ws)?;
    *state.plan.lock().unwrap() = Some(plan.clone());
    Ok(ToolResult::ok(
        "update_plan",
        format!("Plan item #{item} → {}", pstatus.label()),
        Some(plan.render_markdown()),
        None,
    ))
}

// ---------------------------------------------------------------------------
// Semantic search + repo map
// ---------------------------------------------------------------------------

/// Cached TF-IDF index for [`ToolCall::SemanticSearchCodebase`]. Built from
/// plain in-process tokenisation (characters → lowercased identifier/word
/// tokens) — no neural embeddings, works on any machine with zero extra deps.
#[derive(Clone)]
pub struct SemIndex {
    /// Cache key: `root|include|respect_gitignore`, so search reuses the same
    /// build across calls and rebuilds only when the scope changes.
    pub key: String,
    docs: Vec<SemDoc>,
    idf: HashMap<String, f32>,
}

#[derive(Clone)]
struct SemDoc {
    path: PathBuf,
    tokens: HashMap<String, u32>,
    len: f32,
}

impl SemIndex {
    /// Build an index over the given text files (read on the calling task).
    pub fn build(key: String, files: &[PathBuf]) -> Result<SemIndex, String> {
        let mut docs: Vec<SemDoc> = Vec::with_capacity(files.len());
        let mut df: HashMap<String, u32> = HashMap::new();
        for f in files {
            let Ok(text) = std::fs::read_to_string(f) else {
                continue;
            };
            if text.len() > 1_500_000 {
                continue;
            }
            let tokens = tokenize(&text);
            if tokens.is_empty() {
                continue;
            }
            let len = tokens.values().map(|v| *v as f32).sum();
            for t in tokens.keys() {
                *df.entry(t.clone()).or_insert(0) += 1;
            }
            docs.push(SemDoc {
                path: f.clone(),
                tokens,
                len,
            });
        }
        let n = docs.len() as f32;
        let mut idf = HashMap::with_capacity(df.len());
        for (t, d) in df {
            idf.insert(t, (n / (1.0 + d as f32)).ln() + 1.0);
        }
        Ok(SemIndex { key, docs, idf })
    }

    /// Rank documents by cosine similarity against the (tokenized) query.
    pub fn search(&self, query: &str, top_k: usize) -> Vec<(PathBuf, f32, String)> {
        let q = tokenize(query);
        let qn: f32 = q.values().map(|v| *v as f32).sum();
        let mut scored: Vec<(f32, &SemDoc)> = self
            .docs
            .iter()
            .map(|d| {
                let mut dot = 0.0f32;
                for (t, qw) in &q {
                    if let Some(dw) = d.tokens.get(t) {
                        let w = self.idf.get(t).copied().unwrap_or(0.0);
                        dot += (*qw as f32) * w * (*dw as f32) * w;
                    }
                }
                let sim = if qn > 0.0 {
                    dot / (qn.max(qn) * d.len.max(d.len))
                } else {
                    0.0
                };
                (sim, d)
            })
            .collect();
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        scored
            .into_iter()
            .take(top_k)
            .filter(|(s, _)| *s > 0.0)
            .map(|(s, d)| (d.path.clone(), s, snippet(d)))
            .collect()
    }
}

fn tokenize(text: &str) -> HashMap<String, u32> {
    let re = words_re();
    let mut map = HashMap::new();
    for m in re.find_iter(text) {
        let w = m.as_str().to_lowercase();
        if w.len() < 2 {
            continue;
        }
        if STOPWORDS.contains(&w.as_str()) {
            continue;
        }
        *map.entry(w).or_insert(0) += 1;
    }
    map
}

fn snippet(doc: &SemDoc) -> String {
    doc.path.to_string_lossy().into_owned()
}

static WORDS_RE: OnceLock<Regex> = OnceLock::new();
fn words_re() -> &'static Regex {
    WORDS_RE.get_or_init(|| Regex::new(r"[\p{L}\p{N}_]+").unwrap())
}

const STOPWORDS: &[&str] = &[
    "the", "and", "for", "are", "this", "that", "with", "from", "have", "was", "were", "has",
    "its", "not", "but", "you", "your", "all", "can", "will", "just", "would", "there", "their",
    "what", "when", "which", "who", "about", "into", "than", "then", "them", "these", "those",
    "still", "only", "might", "could", "should", "some",
];

async fn semantic_search(
    state: &ToolState,
    root: &Path,
    query: &str,
    include: Option<&str>,
    respect_gitignore: bool,
    top_k: usize,
) -> Result<ToolResult, String> {
    let key = format!(
        "{}|{}|{}",
        root.to_string_lossy(),
        include.unwrap_or(""),
        respect_gitignore
    );

    let index = {
        let mut cache = state.sem_index.lock().unwrap();
        match cache.as_ref() {
            Some(i) if i.key == key => {
                let i = cache.as_ref().unwrap();
                // Need a clone to release the lock before search (no borrow
                // here because we clone documents below via search).
                SemIndex {
                    key: i.key.clone(),
                    docs: i.docs.clone(),
                    idf: i.idf.clone(),
                }
            }
            _ => {
                let files = collect_search_files(root, include, respect_gitignore)?;
                let idx = SemIndex::build(key.clone(), &files)?;
                *cache = Some(SemIndex {
                    key,
                    docs: idx.docs.clone(),
                    idf: idx.idf.clone(),
                });
                idx
            }
        }
    };

    let hits = index.search(query, top_k.min(50));
    if hits.is_empty() {
        return Ok(ToolResult::ok(
            "semantic_search_codebase",
            "No semantic matches".into(),
            Some("No documents matched the query.".into()),
            None,
        ));
    }
    let mut body = String::new();
    for (path, score, _) in &hits {
        body.push_str(&format!("{} (score {score:.3})\n", path.to_string_lossy()));
    }
    Ok(ToolResult::ok(
        "semantic_search_codebase",
        format!("{} match(es)", hits.len()),
        Some(body),
        Some(json!({ "count": hits.len() })),
    ))
}

fn collect_search_files(
    root: &Path,
    include: Option<&str>,
    respect_gitignore: bool,
) -> Result<Vec<PathBuf>, String> {
    let inc = include_set(include);
    let mut files = Vec::new();
    let mut walker = WalkBuilder::new(root);
    walker.hidden(false).parents(false);
    walker
        .git_ignore(respect_gitignore)
        .git_exclude(respect_gitignore)
        .git_global(respect_gitignore);
    walker.standard_filters(respect_gitignore);
    for entry in walker.build() {
        if files.len() >= 4000 {
            break;
        }
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        if !likely_text(entry.path()) {
            continue;
        }
        if let Some(set) = &inc {
            let rel = entry.path().strip_prefix(root).unwrap_or(entry.path());
            let rel_str = rel.to_string_lossy().replace('\\', "/");
            if !set.is_match(&rel_str) && !set.is_match(&format!("/{rel_str}")) {
                continue;
            }
        }
        files.push(entry.into_path());
    }
    Ok(files)
}

/// Repo map payload: ranked symbol graph from tree-sitter definitions +
/// co-occurrence PageRank. Deterministic and dependency-free.
#[derive(Clone)]
pub struct RepoGraph {
    /// Cache key (workspace root).
    pub key: String,
    /// Top-ranked symbols, best first.
    pub symbols: Vec<RepoSymbol>,
}

#[derive(Clone)]
pub struct RepoSymbol {
    pub symbol: String,
    pub kind: String,
    pub path: PathBuf,
    pub line: u32,
    pub score: f32,
}

impl RepoGraph {
    pub fn build(
        key: String,
        root: &Path,
        respect_gitignore: bool,
        top_n: usize,
    ) -> Result<RepoGraph, String> {
        let files = collect_search_files(root, None, respect_gitignore)?;
        let mut defs: Vec<(PathBuf, u32, String, String)> = Vec::new(); // path, line, name, kind
        let mut by_file: HashMap<PathBuf, Vec<String>> = HashMap::new();
        let mut def_file: HashMap<String, (PathBuf, u32, String)> = HashMap::new();
        for f in &files {
            let Some((_, tree, text)) = parse_tree(f)? else {
                continue;
            };
            let names = extract_defs(&tree, &text);
            let mut local = Vec::new();
            for (name, kind, line) in &names {
                let uniq = format!("{}.{}", f.to_string_lossy(), name);
                def_file.insert(uniq.clone(), (f.clone(), *line, kind.clone()));
                defs.push((f.clone(), *line, name.clone(), kind.clone()));
                local.push(uniq);
            }
            by_file.insert(f.clone(), local);
        }

        // Co-occurrence edges: symbols sharing a file get a symmetric edge.
        let all_symbols: Vec<String> = defs
            .iter()
            .map(|(_, _, n, _)| format!("{}.{}", "", n))
            .collect();
        let _ = all_symbols;
        // Better: unique symbol keys.
        let ids: Vec<String> = def_file.keys().cloned().collect();
        let mut adj: HashMap<usize, HashSet<usize>> = HashMap::new();
        let idx_of: HashMap<&String, usize> = ids.iter().enumerate().map(|(i, k)| (k, i)).collect();
        for symbols in by_file.values() {
            for a in symbols {
                for b in symbols {
                    if a == b {
                        continue;
                    }
                    if let (Some(&ia), Some(&ib)) = (idx_of.get(a), idx_of.get(b)) {
                        adj.entry(ia).or_default().insert(ib);
                        adj.entry(ib).or_default().insert(ia);
                    }
                }
            }
        }

        // Iterative PageRank (damping 0.85, 30 iterations).
        let n = ids.len();
        if n == 0 {
            return Ok(RepoGraph {
                key,
                symbols: Vec::new(),
            });
        }
        let mut rank = vec![1.0 / n as f32; n];
        for _ in 0..30 {
            let mut next = vec![0.15 / n as f32; n];
            for (i, r) in rank.iter().enumerate() {
                let out_deg = adj.get(&i).map(|s| s.len()).unwrap_or(0);
                if out_deg == 0 {
                    continue;
                }
                let share = r / out_deg as f32;
                for &j in adj.get(&i).unwrap() {
                    next[j] += 0.85 * share;
                }
            }
            rank = next;
        }

        let mut ranked: Vec<(f32, &String)> =
            ids.iter().zip(rank.iter()).map(|(k, r)| (*r, k)).collect();
        ranked.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        let symbols = ranked
            .into_iter()
            .take(top_n.min(300))
            .filter(|(s, _)| *s > 0.0)
            .map(|(score, key)| {
                let (path, line, kind) = def_file
                    .get(key)
                    .cloned()
                    .unwrap_or_else(|| (PathBuf::new(), 0, String::new()));
                let name = key.rsplit('.').next().unwrap_or("").to_string();
                RepoSymbol {
                    symbol: name,
                    kind,
                    path,
                    line,
                    score,
                }
            })
            .collect();
        Ok(RepoGraph { key, symbols })
    }
}

fn extract_defs(tree: &tree_sitter::Tree, _text: &str) -> Vec<(String, String, u32)> {
    let mut out = Vec::new();
    let cursor = tree.walk();
    fn visit(node: &Node, out: &mut Vec<(String, String, u32)>) {
        let kind = match node.kind() {
            "function_declaration" => Some(("function", "function_declaration")),
            "function_item" => Some(("function", "function_item")),
            "class_declaration" => Some(("class", "class_declaration")),
            "struct_item" => Some(("struct", "struct_item")),
            "enum_item" => Some(("enum", "enum_item")),
            "trait_item" => Some(("trait", "trait_item")),
            "impl_item" => Some(("impl", "impl_item")),
            "mod_item" => Some(("module", "mod_item")),
            "function_definition" => Some(("function", "function_definition")),
            "class_definition" => Some(("class", "class_definition")),
            "method_definition" => Some(("method", "method_definition")),
            "interface_declaration" => Some(("interface", "interface_declaration")),
            "const_item" => Some(("const", "const_item")),
            "static_item" => Some(("static", "static_item")),
            "macro_definition" => Some(("macro", "macro_definition")),
            _ => None,
        };
        if let Some((label, _)) = kind {
            if let Some(name) = node.child_by_field_name("name") {
                if let Ok(name_str) = name.utf8_text(&[]) {
                    out.push((
                        name_str.to_string(),
                        label.to_string(),
                        node.start_position().row as u32 + 1,
                    ));
                }
            }
        }
        let mut w = node.walk();
        for child in node.named_children(&mut w) {
            visit(&child, out);
        }
    }
    visit(&tree.root_node(), &mut out);
    let _ = cursor;
    out
}

async fn view_repo_map(state: &ToolState, root: &Path, top_n: usize) -> Result<ToolResult, String> {
    let key = root.to_string_lossy().into_owned();
    let graph = {
        let mut cache = state.repo_graph.lock().await;
        match cache.get(&key) {
            Some((_mtime, g)) if g.key == key => RepoGraph {
                key: g.key.clone(),
                symbols: g.symbols.clone(),
            },
            _ => {
                let built = RepoGraph::build(key.clone(), root, true, top_n)?;
                let mtime = now_ms();
                cache.insert(
                    key.clone(),
                    (
                        mtime,
                        RepoGraph {
                            key: built.key.clone(),
                            symbols: built.symbols.clone(),
                        },
                    ),
                );
                built
            }
        }
    };

    if graph.symbols.is_empty() {
        return Ok(ToolResult::ok(
            "view_repo_map",
            "No symbols found".into(),
            Some("No tree-sitter definitions were found in this workspace.".into()),
            None,
        ));
    }
    let mut body = String::new();
    for s in &graph.symbols {
        body.push_str(&format!(
            "{}:{}: {} {} (score {:.3})\n",
            s.path.to_string_lossy(),
            s.line,
            s.kind,
            s.symbol,
            s.score
        ));
    }
    Ok(ToolResult::ok(
        "view_repo_map",
        format!("{} symbol(s)", graph.symbols.len()),
        Some(body),
        Some(json!({ "count": graph.symbols.len() })),
    ))
}

/// Extract symbols from the SAT graph. (placeholder to satisfy imports)
fn _unused_sats(_: &RepoGraph) -> usize {
    0
}

// ---------------------------------------------------------------------------
// Lints + tree-sitter queries
// ---------------------------------------------------------------------------

fn read_lints(path: &Path) -> Result<ToolResult, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("Failed to read `{}`: {e}", path.display()))?;
    let mut findings: Vec<String> = Vec::new();

    // TODO / FIXME / HACK / XXX markers.
    let marker_re = Regex::new(r"(?i)\b(TODO|FIXME|HACK|XXX)\b[^\n]*").unwrap();
    for m in marker_re.find_iter(&text) {
        let line = text[..m.start()].matches('\n').count() + 1;
        let snippet = m.as_str().trim();
        if snippet.len() > 160 {
            findings.push(format!("{path:?}:{line}: marker: {}…", &snippet[..160]));
        } else {
            findings.push(format!("{}:{line}: marker: {snippet}", path.display()));
        }
    }

    // Tree-sitter syntax errors when a grammar is available.
    if let Some((_, tree, _)) = parse_tree(path)? {
        collect_syntax_errors(path, &tree, &mut findings);
        collect_cheap_checks(path, &tree, &mut findings);
    }

    if findings.is_empty() {
        return Ok(ToolResult::ok(
            "read_lints",
            format!("No issues in `{}`", path.display()),
            Some("Clean.".into()),
            None,
        ));
    }
    Ok(ToolResult::ok(
        "read_lints",
        format!("{} issue(s)", findings.len()),
        Some(truncate(&findings.join("\n"), 60_000)),
        Some(json!({ "count": findings.len() })),
    ))
}

fn collect_syntax_errors(path: &Path, tree: &tree_sitter::Tree, out: &mut Vec<String>) {
    fn walk(node: &Node, path: &Path, out: &mut Vec<String>) {
        if node.is_error() || node.is_missing() {
            let (l, c) = (
                node.start_position().row + 1,
                node.start_position().column + 1,
            );
            out.push(format!(
                "{}:{l}:{c}: syntax error at `{}`",
                path.display(),
                node.kind()
            ));
        }
        let mut w = node.walk();
        for child in node.named_children(&mut w) {
            walk(&child, path, out);
        }
    }
    walk(&tree.root_node(), path, out);
}

fn collect_cheap_checks(path: &Path, tree: &tree_sitter::Tree, out: &mut Vec<String>) {
    let mut w = tree.walk();
    let mut stack: Vec<Node> = Vec::new();
    let root = tree.root_node();
    stack.push(root);
    while let Some(node) = stack.pop() {
        match node.kind() {
            "debugger_statement" => {
                let (l, c) = (
                    node.start_position().row + 1,
                    node.start_position().column + 1,
                );
                out.push(format!(
                    "{}:{l}:{c}: stray `debugger` statement",
                    path.display()
                ));
            }
            "catch_clause" => {
                if node.named_child_count() < 2 {
                    let (l, c) = (
                        node.start_position().row + 1,
                        node.start_position().column + 1,
                    );
                    out.push(format!(
                        "{}:{l}:{c}: empty catch block swallows errors",
                        path.display()
                    ));
                }
            }
            _ => {}
        }
        let mut cw = node.walk();
        for child in node.named_children(&mut cw) {
            stack.push(child);
        }
    }
    let _ = &mut w;
}

fn tree_sitter_query(path: &Path, query: &str, max_results: usize) -> Result<ToolResult, String> {
    let (lang, tree, text) = match parse_tree(path)? {
        Some(t) => t,
        None => return Err(format!("No tree-sitter grammar for `{}`", path.display())),
    };
    let query = Query::new(&lang, query).map_err(|e| format!("Query error: {e}"))?;
    let mut cursor = QueryCursor::new();
    let mut lines: Vec<String> = Vec::new();
    {
        let mut it = cursor.matches(&query, tree.root_node(), text.as_bytes());
        while lines.len() < max_results {
            let Some(m) = it.next() else { break };
            let Some(first) = m.captures.first() else {
                continue;
            };
            let (start_l, start_c) = (
                first.node.start_position().row + 1,
                first.node.start_position().column + 1,
            );
            let mut parts = Vec::new();
            for cap in m.captures.iter() {
                let name = &query.capture_names()[cap.index as usize];
                if let Ok(t) = cap.node.utf8_text(text.as_bytes()) {
                    parts.push(format!(
                        "{}={}",
                        name,
                        t.chars().take(80).collect::<String>()
                    ));
                }
            }
            lines.push(format!("{start_l}:{start_c}: {}", parts.join(" ")));
        }
    }
    let body = if lines.is_empty() {
        "No captures matched.".to_string()
    } else {
        lines.join("\n")
    };
    Ok(ToolResult::ok(
        "tree_sitter_query",
        format!("{} capture(s)", body.lines().count()),
        Some(body),
        None,
    ))
}

// ---------------------------------------------------------------------------
// Web tools
// ---------------------------------------------------------------------------

fn url_safe(url: &str) -> Result<(), String> {
    let parsed = url::Url::parse(url).map_err(|e| format!("Invalid URL `{url}`: {e}"))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(format!(
            "Only http/https URLs are allowed, got `{}`",
            parsed.scheme()
        ));
    }
    let host = parsed
        .host_str()
        .ok_or_else(|| "URL has no host".to_string())?;
    let host = host.trim_start_matches('[').trim_end_matches(']');
    if host.eq_ignore_ascii_case("localhost")
        || host == "::1"
        || host == "0.0.0.0"
        || host.starts_with("127.")
        || host.starts_with("10.")
        || host.starts_with("192.168.")
        || host.starts_with("169.254.")
        || host.starts_with("fe80:")
        || host.starts_with("fd")
        || host.starts_with("fc")
    {
        return Err("URL resolves to a private/local address (SSRF guard)".into());
    }
    if let Some(octets) = host.split('.').collect::<Vec<_>>().first() {
        if host.split('.').count() == 4 {
            let parts: Vec<u32> = host.split('.').filter_map(|p| p.parse().ok()).collect();
            if parts.len() == 4 {
                let a = parts[0];
                if (a == 172 && (16..=31).contains(&parts[1])) || a == 100 {
                    return Err("URL resolves to a private/local address (SSRF guard)".into());
                }
            }
        }
        let _ = octets;
    }
    Ok(())
}

fn strip_html(html: &str) -> String {
    let script = Regex::new(r"(?is)<script[\s\S]*?</script>").unwrap();
    let style = Regex::new(r"(?is)<style[\s\S]*?</style>").unwrap();
    let tags = Regex::new(r"(?s)<[^>]+>").unwrap();
    let scriptless = script.replace_all(html, " ");
    let styleless = style.replace_all(&scriptless, " ");
    let tagless = tags.replace_all(&styleless, " ");
    let multi = Regex::new(r"[ \t\r\n]+").unwrap();
    let text = multi.replace_all(&tagless, " ").to_string();
    let text = text
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ");
    text.trim().to_string()
}

fn url_decode(s: &str) -> String {
    // Decode %XX and keep '+' as literal space.
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() + 1 && i + 2 <= bytes.len() - 1 {
            if let (Some(a), Some(b)) = (
                hex_val(bytes.get(i + 1).copied()),
                hex_val(bytes.get(i + 2).copied()),
            ) {
                out.push((a << 4) | b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(b: Option<u8>) -> Option<u8> {
    let b = b?;
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn url_encode(s: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        let ok = b.is_ascii_alphanumeric() || b"-_.~".contains(&b);
        if ok {
            out.push(b as char);
        } else {
            out.push('%');
            out.push(HEX[(b >> 4) as usize] as char);
            out.push(HEX[(b & 0x0f) as usize] as char);
        }
    }
    out
}

async fn http_get(url: &str, cap_bytes: usize) -> Result<String, String> {
    url_safe(url)?;
    let client = reqwest::Client::builder()
        .user_agent("ai-editor-agent/0.1")
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| format!("HTTP client: {e}"))?;
    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("GET {url}: {e}"))?;
    let status = resp.status();
    let text = resp.text().await.map_err(|e| format!("Read body: {e}"))?;
    let clipped: String = text.chars().take(cap_bytes).collect();
    if !status.is_success() {
        return Err(format!("HTTP {status} from {url}"));
    }
    Ok(clipped)
}

async fn web_search(query: &str, max_results: usize) -> Result<ToolResult, String> {
    let url = format!("https://html.duckduckgo.com/html/?q={}", url_encode(query));
    let html = http_get(&url, 400_000)
        .await
        .map_err(|e| format!("web_search: {e}"))?;
    let anchors =
        Regex::new(r#"(?is)<a[^>]*class="result__a"[^>]*href="([^"]*)"[^>]*>(.*?)</a>"#).unwrap();
    let snippets = Regex::new(r#"(?is)<a[^>]*class="result__snippet"[^>]*>(.*?)</a>"#).unwrap();
    let snippets: Vec<String> = snippets
        .captures_iter(&html)
        .take(max_results)
        .map(|c| strip_html(c.get(1).map(|m| m.as_str()).unwrap_or("")).to_string())
        .collect();
    let mut results = Vec::new();
    for (i, cap) in anchors.captures_iter(&html).enumerate().take(max_results) {
        let href = cap.get(1).map(|m| m.as_str()).unwrap_or("");
        let title = strip_html(cap.get(2).map(|m| m.as_str()).unwrap_or(""));
        // ddg wraps real URLs in /l/?uddg=<encoded>
        let final_url = if let Some(pos) = href.find("uddg=") {
            let encoded = &href[pos + 5..];
            let encoded = encoded.split('&').next().unwrap_or(encoded);
            url_decode(encoded)
        } else {
            href.to_string()
        };
        let snippet = snippets.get(i).cloned().unwrap_or_default();
        results.push(format!(
            "{}. **{title}**\n   {final_url}\n   {snippet}",
            i + 1
        ));
    }
    if results.is_empty() {
        return Ok(ToolResult::ok(
            "web_search",
            "No results".into(),
            Some("No results found. Try a different query.".into()),
            None,
        ));
    }
    Ok(ToolResult::ok(
        "web_search",
        format!("{} result(s) for `{query}`", results.len()),
        Some(results.join("\n")),
        Some(json!({ "count": results.len() })),
    ))
}

async fn web_extract(url: &str) -> Result<ToolResult, String> {
    let raw = http_get(url, 2_000_000).await?;
    let text = strip_html(&raw);
    let clipped: String = text.chars().take(40_000).collect();
    let count = clipped.chars().count();
    Ok(ToolResult::ok(
        "web_extract",
        format!("Fetched `{url}`"),
        Some(if clipped.is_empty() {
            "(empty page — likely JS-rendered)".to_string()
        } else {
            clipped
        }),
        Some(json!({ "chars": count })),
    ))
}

async fn download_file(url: &str, dst: &Path) -> Result<ToolResult, String> {
    url_safe(url)?;
    let client = reqwest::Client::builder()
        .user_agent("ai-editor-agent/0.1")
        .timeout(Duration::from_secs(120))
        .build()
        .map_err(|e| format!("HTTP client: {e}"))?;
    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("GET {url}: {e}"))?;
    let status = resp.status();
    let bytes = resp.bytes().await.map_err(|e| format!("Read body: {e}"))?;
    if !status.is_success() {
        return Err(format!("HTTP {status} from {url}"));
    }
    if bytes.len() > 200_000_000 {
        return Err("Download exceeds the 200 MB safety cap".into());
    }
    // The destination is policy-scoped upstream (`policy::call_target_paths`
    // lists `DownloadFile`), so an absolute path outside the workspace only
    // reaches here after an explicit approval decision.
    std::fs::write(dst, &bytes).map_err(|e| format!("Failed to write `{}`: {e}", dst.display()))?;
    Ok(ToolResult::ok(
        "download_file",
        format!("Downloaded {} byte(s) to {}", bytes.len(), dst.display()),
        None,
        Some(json!({ "bytes": bytes.len() })),
    ))
}

// ---------------------------------------------------------------------------
// Ask the user
// ---------------------------------------------------------------------------

async fn ask_question(
    state: &ToolState,
    app: &ToolContext,
    question: &str,
    choices: Option<&[String]>,
) -> Result<ToolResult, String> {
    let request_id = state.next_question_id();
    let choices = choices.map(|c| c.to_vec()).unwrap_or_default();
    let (tx, rx) = tokio::sync::oneshot::channel();
    state
        .pending_questions
        .lock()
        .await
        .insert(request_id.clone(), tx);
    let _ = app.emit(
        "agent://question-request",
        QuestionRequestEvent {
            request_id: request_id.clone(),
            question: question.to_string(),
            choices,
            timestamp_ms: now_ms(),
        },
    );
    let answer = tokio::time::timeout(Duration::from_secs(600), rx)
        .await
        .map_err(|_| "The question timed out before the user answered.")?
        .map_err(|_| "The question channel closed before the user answered.")?;
    // Best-effort cleanup in case the UI answered and then closed the channel.
    state.pending_questions.lock().await.remove(&request_id);
    Ok(ToolResult::ok(
        "ask_question",
        "User answered".into(),
        Some(answer),
        None,
    ))
}

// ---------------------------------------------------------------------------
// Human-in-the-loop gate (<hitl_gate>)
// ---------------------------------------------------------------------------

/// Block the agent loop until the user approves or denies a pending action
/// batch. The '<hitl_gate>' reasoning marker before the tool-call batch makes
/// the model pause; this function is what actually freezes execution at that
/// seam. The frontend listens on `agent://hitl-gate` and replies through the
/// `agent_respond_hitl` command. Mapped to `Ok(true)` = approved,
/// `Ok(false)` = denied (loop then reroutes instead of dispatching).
pub async fn hitl_gate(
    state: &ToolState,
    app: &ToolContext,
    session_id: u64,
    reason: &str,
    pending: &[&ToolCall],
) -> Result<bool, String> {
    let request_id = state.next_hitl_id();
    let (tx, rx) = tokio::sync::oneshot::channel();
    state
        .pending_hitl
        .lock()
        .await
        .insert(request_id.clone(), tx);
    let tools: Vec<String> = pending.iter().map(|c| c.summary()).collect();
    let _ = app.emit(
        "agent://hitl-gate",
        HitlGateEvent {
            request_id: request_id.clone(),
            session_id,
            reason: reason.to_string(),
            tools,
            timestamp_ms: now_ms(),
        },
    );
    let decision = tokio::time::timeout(Duration::from_secs(600), rx)
        .await
        .map_err(|_| {
            "The approval request timed out before you decided — treat as denied.".to_string()
        })?
        .map_err(|_| "The approval channel closed before you decided.".to_string())?;
    state.pending_hitl.lock().await.remove(&request_id);
    Ok(decision.approve)
}

// ---------------------------------------------------------------------------
// LLM-backed intelligence tools
// ---------------------------------------------------------------------------

async fn summarize_changes(state: &ToolState, ws: &[PathBuf]) -> Result<ToolResult, String> {
    let root = first_ws(ws).ok_or_else(|| "No workspace set".to_string())?;
    let (_, status_out, _) = run_git(
        &Some(root),
        &["status".into(), "--porcelain=v1".into()],
        Duration::from_secs(60),
    )
    .await?;
    let mut parts = vec![format!("# Uncommitted changes\n\n{status_out}")];
    let (c0, staged, _) = run_git(
        &Some(root),
        &["diff".into(), "--cached".into(), "--stat".into()],
        Duration::from_secs(60),
    )
    .await?;
    let (c1, unstaged, _) = run_git(
        &Some(root),
        &["diff".into(), "--stat".into()],
        Duration::from_secs(60),
    )
    .await?;
    if c0 == 0 && !staged.trim().is_empty() {
        parts.push(format!("\n## Staged\n{staged}"));
    }
    if c1 == 0 && !unstaged.trim().is_empty() {
        parts.push(format!("\n## Unstaged\n{unstaged}"));
    }
    let raw = parts.join("\n");

    if let Some(summary) = llm_brief(
        state,
        "You summarize git changes for a developer. Be concise (≤ 12 lines), use bullets, \
         and note any risks. Only describe what is in the diff.",
        &format!("{raw}\n\nSummary:"),
        320,
    )
    .await
    {
        return Ok(ToolResult::ok(
            "summarize_changes",
            "Summarized uncommitted changes".into(),
            Some(summary),
            None,
        ));
    }

    // No model: deterministic fallback.
    let count = status_out.lines().count();
    Ok(ToolResult::ok(
        "summarize_changes",
        format!("{count} changed path(s)"),
        Some(truncate(&raw, 20_000)),
        Some(json!({ "paths": count })),
    ))
}

async fn analyze_bug(
    state: &ToolState,
    root: Option<&Path>,
    stack: &str,
    path: Option<&Path>,
) -> Result<ToolResult, String> {
    let loc_re = Regex::new(
        r"(?m)([A-Za-z0-9_\.\-/\\]+\.(?:rs|py|ts|tsx|js|go|java|c|cc|cpp|rb)(?::\d+){1,2})",
    )
    .unwrap();
    let mut refs = Vec::new();
    for cap in loc_re.captures_iter(stack) {
        let loc = cap.get(1).unwrap().as_str().to_string();
        if !refs.contains(&loc) {
            refs.push(loc);
        }
    }
    let mut context = String::new();
    for loc in &refs {
        let parts: Vec<&str> = loc.rsplitn(3, ':').collect();
        if parts.len() >= 2 {
            let file = parts[2].to_string();
            let p = PathBuf::from(&file);
            let resolved = if p.is_absolute() {
                p
            } else {
                root.map(|r| r.join(&file)).unwrap_or(p)
            };
            if let Ok(text) = std::fs::read_to_string(&resolved) {
                if let Some(path_focus) = path {
                    let _ = path_focus;
                }
                if let Ok(first) = parts[1].parse::<usize>() {
                    let lines: Vec<&str> = text.lines().collect();
                    let start = first.saturating_sub(8).min(lines.len());
                    let end = (first + 8).min(lines.len());
                    context.push_str(&format!(
                        "\n--- {} (around line {first}) ---\n",
                        resolved.display()
                    ));
                    for (i, l) in lines
                        .iter()
                        .enumerate()
                        .skip(start)
                        .take(end.saturating_sub(start))
                    {
                        context.push_str(&format!("{}: {l}\n", i + 1));
                    }
                }
            }
        }
    }
    if context.is_empty() {
        if let Some(p) = path {
            if let Ok(text) = std::fs::read_to_string(p) {
                context.push_str(&format!("\n--- {} ---\n", p.display()));
                context.push_str(truncate(&text, 12_000).as_str());
                context.push('\n');
            }
        }
    }

    let prompt = format!(
        "STACK TRACE / ERROR:\n{}\n\nRELEVANT CODE:\n{}\n\nDiagnose the root cause and \
         give concrete fix suggestions. Answer with a structured report: **Likely cause**, \
         **Evidence**, **Fix**. Keep it under 30 lines.",
        truncate(stack, 8000),
        truncate(&context, 16_000),
    );

    if let Some(analysis) = llm_brief(
        state,
        "You are an expert debugging assistant. Use only the evidence given. Be precise.",
        &prompt,
        900,
    )
    .await
    {
        return Ok(ToolResult::ok(
            "analyze_bug",
            "Bug analysis complete".into(),
            Some(analysis),
            None,
        ));
    }

    if context.is_empty() {
        return Err("Could not locate any `file:line` references in the stack (no model loaded to analyze further).".into());
    }
    Ok(ToolResult::ok(
        "analyze_bug",
        format!("Located {} reference(s) in the stack", refs.len()),
        Some(truncate(&context, 16_000)),
        Some(json!({ "refs": refs.len() })),
    ))
}

async fn review_code(
    state: &ToolState,
    root: Option<&Path>,
    path: Option<&Path>,
    diff: Option<&str>,
) -> Result<ToolResult, String> {
    let mut content = String::new();
    if let Some(p) = path {
        if p.is_file() {
            content = std::fs::read_to_string(p)
                .map_err(|e| format!("Failed to read `{}`: {e}", p.display()))?;
            content = truncate(&content, 16_000);
        } else {
            return Err(format!("`{}` does not exist", p.display()));
        }
    } else if let Some(d) = diff {
        content = truncate(d, 16_000);
    } else {
        // Default to reviewing the full uncommitted diff.
        let ws_path = root.unwrap_or_else(|| Path::new("."));
        let (c, out, _) =
            run_git(&Some(ws_path), &["diff".into()], Duration::from_secs(60)).await?;
        if c == 0 {
            content = truncate(&out, 16_000);
        }
    }
    if content.trim().is_empty() {
        return Err("Nothing to review: provide a `path` or `diff`, or make changes first.".into());
    }

    let prompt = format!(
        "REVIEW THIS CODE/DIFF:\n\n{}\n\nFind bugs, style issues, and security concerns. \
         Return findings as a numbered list, each entry: `[severity] location: issue + fix`. \
         Severities: CRITICAL / HIGH / MEDIUM / LOW. If the code is clean, say so in one line.",
        content
    );

    let report = llm_brief(
        state,
        "You are a strict senior code reviewer. Be specific and cite line numbers.",
        &prompt,
        1200,
    )
    .await
    .unwrap_or_else(|| {
        "No model loaded — raw code supplied (cannot produce findings without a model).".to_string()
    });

    Ok(ToolResult::ok(
        "review_code",
        "Code review complete".into(),
        Some(report),
        None,
    ))
}

// Helper to attach the auto-checkpoint helper (kept near git tools).
async fn maybe_auto_checkpoint(state: &ToolState, interrupt: &CancellationToken) {
    let first = state
        .step_checkpointed
        .compare_exchange(
            false,
            true,
            std::sync::atomic::Ordering::SeqCst,
            std::sync::atomic::Ordering::SeqCst,
        )
        .is_ok();
    if !first {
        return;
    }
    let Some(ws) = state.primary_workspace().await else {
        return;
    };
    let mut args: Vec<String> = vec!["add".into(), "-A".into()];
    let _ = run_command_checked_owned(
        "git",
        &args,
        Some(&ws),
        Duration::from_secs(120),
        Some(interrupt),
    )
    .await;
    if let Err(_) = run_command_checked_owned(
        "git",
        &args,
        Some(&ws),
        Duration::from_secs(120),
        Some(interrupt),
    )
    .await
    {
        return;
    }
    args = vec!["commit".into(), "-m".into(), "ai: auto-checkpoint".into()];
    let _ = run_command_checked_owned(
        "git",
        &args,
        Some(&ws),
        Duration::from_secs(120),
        Some(interrupt),
    )
    .await;
}
