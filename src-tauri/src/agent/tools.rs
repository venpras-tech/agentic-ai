//! Native tool implementations and the unified execution dispatcher.
//!
//! Every tool follows the same contract: `async fn … -> Result<ToolResult, String>`.
//! The dispatcher wraps them with real-time UI events and a `ToolResult`
//! envelope so the orchestrator gets a uniform response shape regardless of
//! whether the tool succeeded or failed.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use globset::GlobBuilder;
use ignore::WalkBuilder;
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter};
use tokio_util::sync::CancellationToken;
use tree_sitter::{Language, Node, Parser};

use super::{
    now_ms, plan, policy, todo, AgentToolEvent, FileChangedEvent, PermissionDecision,
    PermissionRequestEvent, ToolCall, ToolResult, ToolState,
};
use crate::engine::TextGenerator;

/// How long the agent waits for a human to approve an `ask`-policy tool.
const PERMISSION_TIMEOUT: Duration = Duration::from_secs(120);

/// The outcome of an `ask`-policy gate, including what the user chose so the
/// caller can apply decision memory and record the audit trail.
enum AskOutcome {
    GrantedOnce,
    GrantedSession,
    GrantedAlways,
    Declined,
    TimedOut,
    Aborted,
}

/// Execute a [`ToolCall`], emitting `agent://tool-event` events to the UI.
///
/// Every call is gated by the workspace policy ([`policy::check`]):
///   * `deny`  → rejected immediately with the reason fed back to the model,
///   * `ask`   → a `agent://permission-request` event is emitted and dispatch
///     waits up to `PERMISSION_TIMEOUT` for `agent_respond_permission`,
///   * `allow` → runs as before.
///
/// `interrupt` is the circuit breaker token; terminal sub-processes and MCP
/// calls race against it so an abort kills them within one scheduling tick and
/// returns an "Execution Aborted" [`ToolResult`] instead of hanging.
pub async fn dispatch(
    app: &AppHandle,
    state: &ToolState,
    call: &ToolCall,
    interrupt: CancellationToken,
) -> Result<ToolResult, String> {
    let started = Instant::now();
    let started_at = now_ms();
    let id = state.next_event_id();
    let tool = call.name();

    emit(
        app,
        &AgentToolEvent {
            id: id.clone(),
            tool: tool.to_string(),
            status: "running".into(),
            summary: call.summary(),
            started_at,
            duration_ms: None,
            detail: None,
        },
    );

    // ---- policy gate ----
    let workspace = state.workspace.lock().await.clone();
    let verdict = policy::check(state, call, workspace.as_deref());
    let mut decision = "allow".to_string();
    let allowed = match &verdict {
        policy::Verdict::Allow => true,
        policy::Verdict::Deny(reason) => {
            decision = "deny".to_string();
            let result = ToolResult::err(tool, format!("`{tool}` blocked"), reason.clone());
            let duration_ms = started.elapsed().as_millis() as u64;
            emit(
                app,
                &AgentToolEvent {
                    id: id.clone(),
                    tool: tool.to_string(),
                    status: "error".into(),
                    summary: result.summary.clone(),
                    started_at,
                    duration_ms: Some(duration_ms),
                    detail: result.error.clone(),
                },
            );
            audit(
                state,
                workspace.as_deref(),
                &id,
                tool,
                &call.summary(),
                &decision,
                started_at,
                duration_ms,
                Some(false),
                Some(reason.as_str()),
            );
            return Ok(result);
        }
        policy::Verdict::Ask { request_id } => {
            match ask_approval(app, state, request_id, tool, call.summary(), &interrupt).await {
                AskOutcome::GrantedOnce => {
                    decision = "granted".to_string();
                    true
                }
                AskOutcome::GrantedSession => {
                    decision = "granted-session".to_string();
                    policy::remember_session(state, call);
                    true
                }
                AskOutcome::GrantedAlways => {
                    decision = "granted-always".to_string();
                    let _ = policy::remember_always(workspace.as_deref(), call);
                    true
                }
                AskOutcome::Declined => {
                    decision = "declined".to_string();
                    false
                }
                AskOutcome::TimedOut => {
                    decision = "timed-out".to_string();
                    false
                }
                AskOutcome::Aborted => {
                    decision = "aborted".to_string();
                    false
                }
            }
        }
    };
    if !allowed {
        let msg = "Tool call was not approved by the user.".to_string();
        let result = ToolResult::err(tool, format!("`{tool}` denied"), msg.clone());
        let duration_ms = started.elapsed().as_millis() as u64;
        emit(
            app,
            &AgentToolEvent {
                id: id.clone(),
                tool: tool.to_string(),
                status: "error".into(),
                summary: result.summary.clone(),
                started_at,
                duration_ms: Some(duration_ms),
                detail: result.error.clone(),
            },
        );
        audit(
            state,
            workspace.as_deref(),
            &id,
            tool,
            &call.summary(),
            &decision,
            started_at,
            duration_ms,
            Some(false),
            Some(msg.as_str()),
        );
        return Ok(result);
    }

    let result = match call {
        ToolCall::GlobSearchCodebase {
            pattern,
            root,
            respect_gitignore,
        } => {
            glob_search_codebase(
                state,
                pattern,
                root.as_deref(),
                respect_gitignore.unwrap_or(true),
            )
            .await
        }
        ToolCall::ViewFileStructure { path, max_depth } => {
            view_file_structure(path, max_depth.unwrap_or(4)).await
        }
        ToolCall::ReadFileRange {
            path,
            start_line,
            end_line,
        } => read_file_range(path, *start_line, *end_line).await,
        ToolCall::ApplyFileDiff { path, diff } => apply_file_diff(app, path, diff).await,
        ToolCall::WriteFile { path, content } => write_file(app, path, content).await,
        ToolCall::SearchFileContents {
            pattern,
            include,
            root,
            respect_gitignore,
        } => {
            search_file_contents(
                state,
                pattern,
                include.as_deref(),
                root.as_deref(),
                respect_gitignore.unwrap_or(true),
            )
            .await
        }
        ToolCall::SemanticSearchCodebase {
            query,
            include,
            root,
            respect_gitignore,
            top_k,
        } => {
            semantic_search_codebase(
                state,
                query,
                include.as_deref(),
                root.as_deref(),
                respect_gitignore.unwrap_or(true),
                top_k.unwrap_or(10),
            )
            .await
        }
        ToolCall::CreateSkill {
            name,
            description,
            content,
        } => create_skill(app, state, name, description.as_deref(), content).await,
        ToolCall::ReadSkill { name } => read_skill(state, name).await,
        ToolCall::ExecuteTerminalCommand {
            command,
            timeout_secs,
            cwd,
        } => {
            execute_terminal_command(
                app,
                state,
                &interrupt,
                command,
                *timeout_secs,
                cwd.as_deref(),
            )
            .await
        }
        ToolCall::CallMcpTool {
            server,
            server_bin,
            server_args,
            tool,
            arguments,
            timeout_secs,
        } => {
            call_mcp_tool(
                app,
                state,
                &interrupt,
                server.as_deref(),
                server_bin.as_deref(),
                server_args,
                tool,
                arguments,
                *timeout_secs,
            )
            .await
        }
        ToolCall::GitStatus { .. } => {
            git_capture(state, &interrupt, &["status", "--short", "--branch"], None).await
        }
        ToolCall::GitDiff { path } => git_diff(state, &interrupt, path.as_deref()).await,
        ToolCall::GitCommit { message } => git_commit(state, &interrupt, message).await,
        ToolCall::GitCheckpoint { message } => {
            git_checkpoint(state, &interrupt, message.as_deref()).await
        }
        ToolCall::GitRevert { commit } => git_revert(state, &interrupt, commit.as_deref()).await,
        ToolCall::RunTests { command } => {
            run_tests(app, state, &interrupt, command.as_deref()).await
        }
        ToolCall::CreatePlan { title, goal, items } => create_plan(state, title, goal, items).await,
        ToolCall::ReadPlan {} => read_plan(state).await,
        ToolCall::UpdatePlan {
            item,
            status,
            details,
        } => update_plan(state, *item, status, details.as_deref()).await,
        // ExecutePlan is intercepted by the orchestrator before dispatch; treat as unreachable.
        ToolCall::ExecutePlan {} => Ok(ToolResult::err(
            "execute_plan",
            "execute_plan is handled by the orchestrator".into(),
            "should not reach dispatch".into(),
        )),
        ToolCall::ListDir { path } => list_dir(state, path.as_deref()).await,
        ToolCall::ReadFileChars {
            path,
            offset,
            limit,
        } => {
            read_file_chars(
                path,
                offset.unwrap_or(0),
                limit.unwrap_or(DEFAULT_READ_CHARS),
            )
            .await
        }
        ToolCall::CreateFolder { path } => create_folder(state, path).await,
        ToolCall::CopyFileOrFolder {
            src,
            dst,
            can_overwrite,
        } => copy_file_or_folder(state, src, dst, can_overwrite.unwrap_or(false)).await,
        ToolCall::MoveFileOrFolder {
            src,
            dst,
            can_overwrite,
        } => move_file_or_folder(state, src, dst, can_overwrite.unwrap_or(false)).await,
        ToolCall::DeleteFileOrFolder { path } => delete_file_or_folder(state, path).await,
        ToolCall::GetScratchpadFolder {} => get_scratchpad_folder(state).await,
        ToolCall::SetTodoList { items } => set_todo_list(app, state, items).await,
        ToolCall::GetTodoList {} => get_todo_list(state).await,
        ToolCall::MarkTodoItemDone { item } => mark_todo_item_done(app, state, *item).await,
        ToolCall::WebSearch { query, max_results } => {
            web_search(state, &interrupt, query, max_results.unwrap_or(8)).await
        }
        ToolCall::WebExtract { url } => web_extract(state, &interrupt, url).await,
        ToolCall::DownloadFile { url, path } => {
            download_file_tool(state, &interrupt, url, path).await
        }
        ToolCall::RunPython { code, timeout_secs } => {
            run_python(state, &interrupt, code, *timeout_secs).await
        }
        ToolCall::RunJavascript { code, timeout_secs } => {
            run_javascript(state, &interrupt, code, *timeout_secs).await
        }
        ToolCall::Calculate { expression } => match eval_arithmetic(expression) {
            Ok(value) => Ok(ToolResult::ok(
                "calculate",
                format!("{expression} = {value}"),
                Some(value.to_string()),
                None,
            )),
            Err(e) => Err(format!("calculate: {e}")),
        },
        ToolCall::ListMcpServers {} => list_mcp_servers(app).await,
        ToolCall::AddMcpServer { name, bin, args } => add_mcp_server(app, name, bin, args).await,
        ToolCall::RemoveMcpServer { name } => remove_mcp_server(app, state, name).await,
        ToolCall::AttachFile { path } => attach_file(state, path).await,
        ToolCall::SearchAttachedFiles { query, top_k } => {
            search_attached_files(state, query, *top_k).await
        }
        ToolCall::DetachFile { path } => detach_file(state, path).await,
        ToolCall::TranscribeAudio { path, language } => {
            transcribe_audio_tool(state, path, language.as_deref()).await
        }
    };

    let duration_ms = started.elapsed().as_millis() as u64;
    let final_result = match result {
        Ok(mut r) => {
            r.duration_ms = duration_ms;
            r
        }
        Err(e) => {
            let mut r = ToolResult::err(tool, format!("`{tool}` failed"), e);
            r.duration_ms = duration_ms;
            r
        }
    };

    emit(
        app,
        &AgentToolEvent {
            id: id.clone(),
            tool: tool.to_string(),
            status: if final_result.success {
                "done".into()
            } else {
                "error".into()
            },
            summary: final_result.summary.clone(),
            started_at,
            duration_ms: Some(duration_ms),
            detail: final_result.error.clone(),
        },
    );

    audit(
        state,
        workspace.as_deref(),
        &id,
        tool,
        &call.summary(),
        &decision,
        started_at,
        duration_ms,
        Some(final_result.success),
        final_result.error.as_deref(),
    );

    Ok(final_result)
}

/// Append one line to `{workspace}/.ai/audit.jsonl` describing a tool call's
/// policy decision and outcome. Best effort: a missing/read-only workspace or
/// disk error never breaks the agent loop. Only the human-readable summary is
/// logged - raw args (file content, secrets) are never written.
#[allow(clippy::too_many_arguments)]
fn audit(
    state: &ToolState,
    workspace: Option<&Path>,
    id: &str,
    tool: &str,
    summary: &str,
    decision: &str,
    started_at: u64,
    duration_ms: u64,
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
    let entry = json!({
        "ts": now_ms(),
        "id": id,
        "tool": tool,
        "summary": summary,
        "decision": decision,
        "startedAt": started_at,
        "latencyMs": duration_ms,
        "success": success,
        "error": error,
    });
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        use std::io::Write;
        let _ = writeln!(f, "{entry}");
    }
}

/// Emit a `agent://permission-request` and wait for the user's decision.
async fn ask_approval(
    app: &AppHandle,
    state: &ToolState,
    request_id: &str,
    tool: &str,
    summary: String,
    interrupt: &CancellationToken,
) -> AskOutcome {
    // Independent LLM shell-approval reviewer pass (Bionic §3.3): for terminal
    // commands, ask the model for a one-line SAFE/UNSAFE second opinion and
    // surface it in the approval dialog. Best effort — any failure yields None
    // and the dialog simply shows no review.
    let review = if tool == "execute_terminal_command" {
        shell_review(state, &summary, interrupt).await
    } else {
        None
    };

    let (tx, rx) = tokio::sync::oneshot::channel();
    {
        let mut reqs = state.permission_requests.lock().await;
        reqs.insert(request_id.to_string(), tx);
    }
    crate::logging::warn(
        None,
        "tool.permission",
        &format!("waiting for approval — {tool}: {summary}"),
    );
    let _ = app.emit(
        "agent://permission-request",
        PermissionRequestEvent {
            request_id: request_id.to_string(),
            tool: tool.to_string(),
            summary,
            timestamp_ms: now_ms(),
            review,
        },
    );

    enum Rcvd {
        Decision(PermissionDecision),
        TimedOut,
        Aborted,
    }
    let rcvd = tokio::select! {
        r = rx => Rcvd::Decision(r.unwrap_or(PermissionDecision::Deny)),
        _ = tokio::time::sleep(PERMISSION_TIMEOUT) => Rcvd::TimedOut,
        _ = interrupt.clone().cancelled_owned() => Rcvd::Aborted,
    };
    let mut reqs = state.permission_requests.lock().await;
    reqs.remove(request_id);

    match rcvd {
        Rcvd::Decision(PermissionDecision::AllowOnce) => {
            crate::logging::info(None, "tool.permission", "allowed (once)");
            AskOutcome::GrantedOnce
        }
        Rcvd::Decision(PermissionDecision::AllowSession) => {
            crate::logging::info(None, "tool.permission", "allowed (session)");
            AskOutcome::GrantedSession
        }
        Rcvd::Decision(PermissionDecision::AlwaysAllow) => {
            crate::logging::info(None, "tool.permission", "allowed (always)");
            AskOutcome::GrantedAlways
        }
        Rcvd::Decision(PermissionDecision::Deny) => {
            crate::logging::warn(None, "tool.permission", "denied by user");
            AskOutcome::Declined
        }
        Rcvd::TimedOut => {
            crate::logging::warn(None, "tool.permission", "timed out");
            AskOutcome::TimedOut
        }
        Rcvd::Aborted => {
            crate::logging::warn(None, "tool.permission", "aborted");
            AskOutcome::Aborted
        }
    }
}

/// Independent LLM review of a shell command (Bionic §3.3 hardening).
///
/// Runs a tiny generation ("VERDICT: SAFE/UNSAFE — reason") on a spare engine
/// worker while the approval dialog is open. Constraints:
///   * needs a pool with ≥2 workers so the reviewer can never collide with
///     the main agent loop (or parallel subtasks) on the same generator,
///   * streams to session id 0 (no live chat message owns 0), so review
///     tokens never render in the timeline,
///   * hard 45s cap; any failure returns None (review is advisory only).
async fn shell_review(
    state: &ToolState,
    command_summary: &str,
    _interrupt: &CancellationToken,
) -> Option<String> {
    let pool = state.engine.lock().await.clone()?;
    if pool.len() < 2 {
        return None;
    }
    let tx = state.worker_tx.lock().unwrap().clone()?;
    let mut gen = pool.handle(pool.len() - 1);
    let prompt = format!(
        "You are a shell-command safety reviewer. The coding assistant wants to \
run the command below. Reply with EXACTLY one line starting with either:\n\
VERDICT: SAFE - <short reason>\nor\n\
VERDICT: UNSAFE - <short reason>\n\
Consider data loss, destructive flags, network side effects and scope.\n\n\
Command: {command_summary}\n"
    );
    let request = crate::engine::InferenceRequest {
        prompt,
        max_tokens: 48,
        temperature: Some(0.1),
        top_p: Some(0.9),
        seed: None,
        stop_words: Some(vec!["<|endoftext|>".into()]),
    };
    // PoolGenerator::generate blocks on a crossbeam reply — keep it off the
    // async runtime thread.
    let result = tokio::time::timeout(
        Duration::from_secs(45),
        tokio::task::spawn_blocking(move || {
            gen.generate(&request, 0, &CancellationToken::new(), &tx)
        }),
    )
    .await
    .ok()?
    .ok()?;

    match result {
        Ok(outcome) => {
            let line = outcome.full_text.lines().find(|l| l.contains("VERDICT"))?;
            let cleaned = line.trim_start_matches(['-', '*', ' ']).trim().to_string();
            (!cleaned.is_empty()).then_some(cleaned)
        }
        Err(_) => None,
    }
}

/// Compute a unified diff between two strings (best effort).
fn unified_diff(a: &str, b: &str, path: &str) -> Option<String> {
    use similar::TextDiff;
    let diff = TextDiff::from_lines(a, b);
    if diff.iter_all_changes().count() == 0 {
        return None;
    }
    let mut out = String::new();
    for hunk in diff
        .unified_diff()
        .context_radius(2)
        .header(path, path)
        .iter_hunks()
    {
        out.push_str(&hunk.to_string());
        out.push('\n');
    }
    if out.trim().is_empty() {
        None
    } else {
        Some(out)
    }
}

/// Emit `agent://file-changed` so the frontend can sync open editors + show a
/// diff preview.
fn emit_file_changed(app: &AppHandle, path: &str, kind: &str, before: &str, after: &str) {
    let _ = app.emit(
        "agent://file-changed",
        FileChangedEvent {
            path: path.to_string(),
            kind: kind.to_string(),
            diff: unified_diff(before, after, path),
        },
    );
}

fn emit(app: &AppHandle, event: &AgentToolEvent) {
    let _ = app.emit("agent://tool-event", event);
    // Console mirror of the tool lifecycle (see `logging`): one line per
    // state transition so background agent work is visible in the terminal.
    match event.status.as_str() {
        "running" => crate::logging::info(
            None,
            "tool",
            &format!("▶ {} — {}", event.tool, event.summary),
        ),
        "done" => crate::logging::info(
            None,
            "tool",
            &format!("✓ {} ({} ms)", event.tool, event.duration_ms.unwrap_or(0)),
        ),
        _ => crate::logging::warn(
            None,
            "tool",
            &format!(
                "✖ {} ({}) {}",
                event.tool,
                event.status,
                event.detail.as_deref().unwrap_or("failed")
            ),
        ),
    }
}

async fn resolve_root(state: &ToolState, root: Option<&str>) -> Result<PathBuf, String> {
    if let Some(r) = root {
        let p = PathBuf::from(r);
        if p.is_dir() {
            return Ok(p);
        }
        return Err(format!(
            "Root path does not exist or is not a directory: {r}"
        ));
    }
    let guard = state.workspace.lock().await;
    match guard.as_ref() {
        Some(p) => Ok(p.clone()),
        None => Err(
            "No workspace set yet - open a workspace first, or pass an explicit `root`."
                .to_string(),
        ),
    }
}

fn rel_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

// ---------------------------------------------------------------------------
// glob_search_codebase
// ---------------------------------------------------------------------------

const SKIP_DIRS: &[&str] = &[
    "node_modules",
    ".git",
    "target",
    "dist",
    "build",
    ".next",
    ".venv",
    "venv",
    ".cache",
    "vendor",
    "__pycache__",
];

const MAX_GLOB_RESULTS: usize = 500;

async fn glob_search_codebase(
    state: &ToolState,
    pattern: &str,
    root: Option<&str>,
    respect_gitignore: bool,
) -> Result<ToolResult, String> {
    let root = resolve_root(state, root).await?;
    let matcher = GlobBuilder::new(pattern)
        .case_insensitive(true)
        .build()
        .map_err(|e| format!("Invalid glob `{pattern}`: {e}"))?
        .compile_matcher();

    let mut matches: Vec<PathBuf> = Vec::new();
    let mut builder = WalkBuilder::new(&root);
    builder
        .hidden(true)
        .parents(true)
        .ignore(true)
        .git_ignore(respect_gitignore)
        .git_global(respect_gitignore)
        .git_exclude(respect_gitignore)
        .require_git(false)
        .follow_links(false);
    builder.filter_entry(|entry| {
        if entry.depth() == 0 {
            return true;
        }
        if entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false) {
            let name = entry.file_name().to_string_lossy();
            return !SKIP_DIRS.contains(&name.as_ref());
        }
        true
    });

    for entry in builder.build() {
        let entry = entry.map_err(|e| format!("Walk error: {e}"))?;
        if !entry.file_type().map(|ft| ft.is_file()).unwrap_or(false) {
            continue;
        }
        let path = entry.path();
        let basename = path
            .file_name()
            .map(|f| f.to_string_lossy())
            .unwrap_or_default();
        if matcher.is_match(rel_path(&root, path)) || matcher.is_match(basename.as_ref()) {
            matches.push(path.to_path_buf());
            if matches.len() >= MAX_GLOB_RESULTS {
                break;
            }
        }
    }

    matches.sort();
    let mut stdout = String::new();
    if matches.is_empty() {
        stdout.push_str("No files matched the pattern.\n");
    } else {
        for p in &matches {
            stdout.push_str(&rel_path(&root, p));
            stdout.push('\n');
        }
    }

    let count = matches.len();
    let summary = if count >= MAX_GLOB_RESULTS {
        format!("Found {count} files (truncated to first {MAX_GLOB_RESULTS})")
    } else if count == 0 {
        format!("No files matched `{pattern}`")
    } else {
        format!("Found {count} file(s) matching `{pattern}`")
    };

    Ok(ToolResult::ok(
        "glob_search_codebase",
        summary,
        Some(stdout),
        Some(json!({
            "matched": count,
            "root": root.to_string_lossy(),
            "pattern": pattern,
        })),
    ))
}

// ---------------------------------------------------------------------------
// search_file_contents — regex search across matching files
// ---------------------------------------------------------------------------

const MAX_SEARCH_RESULTS: usize = 200;
const MAX_SEARCH_FILES: usize = 300;
const MAX_SEARCH_FILE_SIZE: u64 = 512 * 1024;

async fn search_file_contents(
    state: &ToolState,
    pattern: &str,
    include: Option<&str>,
    root: Option<&str>,
    respect_gitignore: bool,
) -> Result<ToolResult, String> {
    let root = resolve_root(state, root).await?;
    let re = regex::RegexBuilder::new(pattern)
        .case_insensitive(true)
        .build()
        .map_err(|e| format!("Invalid regex `{pattern}`: {e}"))?;

    let include_matcher = include
        .filter(|inc| !inc.trim().is_empty())
        .map(|inc| {
            GlobBuilder::new(inc)
                .case_insensitive(true)
                .build()
                .map(|g| g.compile_matcher())
        })
        .transpose()
        .map_err(|e| format!("Invalid include glob `{include:?}`: {e}"))?;

    let mut builder = WalkBuilder::new(&root);
    builder
        .hidden(true)
        .parents(true)
        .ignore(true)
        .git_ignore(respect_gitignore)
        .git_global(respect_gitignore)
        .git_exclude(respect_gitignore)
        .require_git(false)
        .follow_links(false);
    builder.filter_entry(|entry| {
        if entry.depth() == 0 {
            return true;
        }
        if entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false) {
            let name = entry.file_name().to_string_lossy();
            return !SKIP_DIRS.contains(&name.as_ref());
        }
        true
    });

    use tokio::io::AsyncBufReadExt;
    let mut matches: Vec<(String, usize, String)> = Vec::new();
    let mut files_searched = 0usize;

    for entry in builder.build() {
        let entry = entry.map_err(|e| format!("Walk error: {e}"))?;
        if files_searched >= MAX_SEARCH_FILES {
            break;
        }
        let ft = match entry.file_type() {
            Some(ft) if ft.is_file() => ft,
            _ => continue,
        };
        if ft.is_symlink() {
            continue;
        }
        let path = entry.path();
        let rel = rel_path(&root, path);
        if let Some(m) = &include_matcher {
            if !m.is_match(&rel) {
                continue;
            }
        }
        let Ok(meta) = std::fs::metadata(path) else {
            continue;
        };
        if meta.len() > MAX_SEARCH_FILE_SIZE {
            continue;
        }
        files_searched += 1;

        let Ok(file) = tokio::fs::File::open(path).await else {
            continue;
        };
        let mut reader = tokio::io::BufReader::new(file);
        let mut line = String::new();
        let mut lineno = 0usize;
        loop {
            line.clear();
            match reader.read_line(&mut line).await {
                Ok(0) => break,
                Ok(_) => {}
                Err(_) => break,
            }
            lineno += 1;
            if matches.len() >= MAX_SEARCH_RESULTS {
                break;
            }
            if line.len() > 2_000 {
                continue;
            }
            if re.is_match(&line) {
                let trimmed = line.trim_end().to_string();
                matches.push((rel.clone(), lineno, trimmed));
            }
        }
        if matches.len() >= MAX_SEARCH_RESULTS {
            break;
        }
    }

    let truncated = matches.len() >= MAX_SEARCH_RESULTS;
    matches.truncate(MAX_SEARCH_RESULTS);

    let mut out = String::new();
    let count = matches.len();
    for (rel, ln, text) in &matches {
        let disp = if text.len() > 300 {
            format!("{}…", &text[..300])
        } else {
            text.clone()
        };
        out.push_str(&format!("{rel}:{ln}: {disp}\n"));
    }

    let summary = if truncated {
        format!("Found {count}+ matches for `{pattern}` (capped at {MAX_SEARCH_RESULTS})")
    } else if count == 0 {
        format!("No matches for `{pattern}` in {} file(s)", files_searched)
    } else {
        format!("Found {count} match(es) for `{pattern}` across {files_searched} file(s)")
    };

    Ok(ToolResult::ok(
        "search_file_contents",
        summary,
        Some(out),
        Some(json!({
            "matched": count,
            "filesSearched": files_searched,
            "root": root.to_string_lossy(),
            "pattern": pattern,
            "truncated": truncated,
        })),
    ))
}

// ---------------------------------------------------------------------------
// semantic_search_codebase
// ---------------------------------------------------------------------------

/// Stopwords dropped from both index and query tokens. Code tokens (identifiers
/// like `auth`, `login`, `token`) are kept; only high-frequency connectors are
/// removed so ranking stays code-aware.
const SEM_STOPWORDS: &[&str] = &[
    "the", "and", "are", "for", "not", "but", "with", "this", "that", "from", "have", "has", "was",
    "were", "you", "your", "will", "into", "than", "then", "them", "their", "been", "being",
    "about", "would", "could", "should", "there", "here", "when", "where", "which", "while",
    "after", "before", "also", "over", "under", "each", "between", "within", "above", "such",
    "only", "very", "just", "can", "make", "make", "used", "use", "using", "does", "doing", "done",
    "does", "was", "its", "it's", "our", "out", "all", "any", "both", "few", "more", "most",
];

/// Split a token stream into lowercase word tokens, keeping meaningful code
/// identifiers. Handles camelCase / snake_case / UPPER segments and drops
/// stopwords + 1-2 char noise.
fn sem_tokens(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for seg in text.split(|c: char| !c.is_alphanumeric()) {
        if seg.is_empty() {
            continue;
        }
        // Split camelCase boundaries: lower→Upper, digit→letter.
        let mut current = String::new();
        let chars: Vec<char> = seg.chars().collect();
        for i in 0..chars.len() {
            let c = chars[i];
            if i > 0
                && c.is_uppercase()
                && !current.is_empty()
                && (chars[i - 1].is_lowercase() || chars[i - 1].is_ascii_digit())
                && !current.is_empty()
            {
                out.push(current.to_lowercase());
                current.clear();
            }
            current.push(c);
        }
        if !current.is_empty() {
            out.push(current.to_lowercase());
        }
    }
    out.into_iter()
        .filter(|t| t.len() >= 2 && !SEM_STOPWORDS.contains(&t.as_str()))
        .collect()
}

/// One indexed chunk: a window of lines from a file.
struct SemChunk {
    /// Relative path.
    path: String,
    /// 1-based start line of the window.
    start_line: usize,
    /// TF vector of token → count within the window.
    tf: HashMap<String, usize>,
}

/// Build the in-memory TF-IDF index over the workspace and rank windows by
/// cosine similarity to `query`. Fully local — no external embeddings model.
async fn semantic_search_codebase(
    state: &ToolState,
    query: &str,
    include: Option<&str>,
    root: Option<&str>,
    respect_gitignore: bool,
    top_k: usize,
) -> Result<ToolResult, String> {
    let root = resolve_root(state, root).await?;
    let include_matcher = include
        .filter(|inc| !inc.trim().is_empty())
        .map(|inc| {
            GlobBuilder::new(inc)
                .case_insensitive(true)
                .build()
                .map(|g| g.compile_matcher())
        })
        .transpose()
        .map_err(|e| format!("Invalid include glob `{include:?}`: {e}"))?;

    let mut builder = WalkBuilder::new(&root);
    builder
        .hidden(true)
        .parents(true)
        .ignore(true)
        .git_ignore(respect_gitignore)
        .git_global(respect_gitignore)
        .git_exclude(respect_gitignore)
        .require_git(false)
        .follow_links(false);
    builder.filter_entry(|entry| {
        if entry.depth() == 0 {
            return true;
        }
        if entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false) {
            let name = entry.file_name().to_string_lossy();
            return !SKIP_DIRS.contains(&name.as_ref());
        }
        true
    });

    const WINDOW_LINES: usize = 40;
    const WINDOW_STEP: usize = 20;
    const MAX_FILES: usize = 600;
    const MAX_CHUNKS: usize = 4000;

    let mut chunks: Vec<SemChunk> = Vec::new();
    let mut files_indexed = 0usize;

    for entry in builder.build() {
        let entry = entry.map_err(|e| format!("Walk error: {e}"))?;
        if files_indexed >= MAX_FILES || chunks.len() >= MAX_CHUNKS {
            break;
        }
        let ft = match entry.file_type() {
            Some(ft) if ft.is_file() => ft,
            _ => continue,
        };
        if ft.is_symlink() {
            continue;
        }
        let path = entry.path();
        let rel = rel_path(&root, path);
        if let Some(m) = &include_matcher {
            if !m.is_match(&rel) {
                continue;
            }
        }
        let Ok(meta) = std::fs::metadata(path) else {
            continue;
        };
        if meta.len() > MAX_SEARCH_FILE_SIZE {
            continue;
        }
        files_indexed += 1;

        let Ok(bytes) = tokio::fs::read(path).await else {
            continue;
        };
        let text = String::from_utf8_lossy(&bytes);
        let lines: Vec<&str> = text.lines().collect();
        if lines.is_empty() {
            continue;
        }
        let mut start = 0usize;
        while start < lines.len() && chunks.len() < MAX_CHUNKS {
            let end = (start + WINDOW_LINES).min(lines.len());
            let window = lines[start..end].join("\n");
            let tf = count_tf(&window);
            if !tf.is_empty() {
                chunks.push(SemChunk {
                    path: rel.clone(),
                    start_line: start + 1,
                    tf,
                });
            }
            if end == lines.len() {
                break;
            }
            start += WINDOW_STEP;
        }
    }

    if chunks.is_empty() {
        return Ok(ToolResult::ok(
            "semantic_search_codebase",
            format!("No indexable files under `{}`", root.to_string_lossy()),
            Some(String::new()),
            Some(json!({ "matches": 0, "root": root.to_string_lossy() })),
        ));
    }

    // IDF: log(N / df).
    let n = chunks.len() as f64;
    let mut df: HashMap<&str, usize> = HashMap::new();
    for c in &chunks {
        let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for t in c.tf.keys() {
            if seen.insert(t) {
                *df.entry(t.as_str()).or_insert(0) += 1;
            }
        }
    }

    // Query vector.
    let q_tokens = sem_tokens(query);
    let mut q_tf: HashMap<&str, usize> = HashMap::new();
    for t in &q_tokens {
        *q_tf.entry(t.as_str()).or_insert(0) += 1;
    }
    let q_vec: Vec<(&str, f64)> = q_tf
        .iter()
        .filter(|(t, _)| df.contains_key(*t))
        .map(|(t, &f)| {
            let idf = (n / (*df.get(t).unwrap() as f64)).ln() + 1.0;
            (*t, f as f64 * idf)
        })
        .collect();

    if q_vec.is_empty() {
        return Ok(ToolResult::ok(
            "semantic_search_codebase",
            format!("No overlap between query `{query}` and the indexed codebase"),
            Some(String::new()),
            Some(json!({ "matches": 0, "root": root.to_string_lossy() })),
        ));
    }

    // Score chunks by cosine similarity of TF-IDF vectors.
    let mut scored: Vec<(f64, &SemChunk)> = Vec::with_capacity(chunks.len());
    for c in &chunks {
        let mut dot = 0.0;
        let mut c_norm = 0.0;
        for (t, &count) in &c.tf {
            let idf = (n / (*df.get(t.as_str()).unwrap() as f64)).ln() + 1.0;
            let w = count as f64 * idf;
            c_norm += w * w;
            if let Some((_, qw)) = q_vec.iter().find(|(qt, _)| qt == t) {
                dot += qw * w;
            }
        }
        if c_norm > 0.0 && dot > 0.0 {
            scored.push((dot / c_norm.sqrt(), c));
        }
    }

    let mut q_norm_sq = 0.0;
    for (_, w) in &q_vec {
        q_norm_sq += w * w;
    }
    let q_norm = q_norm_sq.sqrt();
    if q_norm > 0.0 {
        for (s, _) in scored.iter_mut() {
            *s /= q_norm;
        }
    }

    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

    const SEM_MAX_RESULTS: usize = 25;
    let total = scored.len();
    let k = top_k.min(SEM_MAX_RESULTS).min(total);
    let mut out = String::new();
    for (rank, (score, c)) in scored.iter().take(k).enumerate() {
        out.push_str(&format!(
            "{:>2}. {:.2}  {}:{}:{}\n",
            rank + 1,
            score,
            c.path,
            c.start_line,
            c.start_line + WINDOW_LINES - 1
        ));
    }

    let summary = format!(
        "Semantic search `{query}` — {total} matching region(s), showing top {k} (indexed {files_indexed} file(s))"
    );
    Ok(ToolResult::ok(
        "semantic_search_codebase",
        summary,
        Some(out),
        Some(json!({
            "matches": total,
            "filesIndexed": files_indexed,
            "root": root.to_string_lossy(),
            "topK": k,
            "query": query,
        })),
    ))
}

fn count_tf(text: &str) -> HashMap<String, usize> {
    let mut tf: HashMap<String, usize> = HashMap::new();
    for t in sem_tokens(text) {
        *tf.entry(t).or_insert(0) += 1;
    }
    tf
}

// ---------------------------------------------------------------------------
// view_file_structure
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
struct Def {
    kind: String,
    name: String,
    sig: String,
    line: usize,
    end_line: usize,
}

async fn view_file_structure(path: &str, max_depth: usize) -> Result<ToolResult, String> {
    let src = tokio::fs::read(path)
        .await
        .map_err(|e| format!("Cannot read `{path}`: {e}"))?;
    let lang: Language = tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into();
    let mut parser = Parser::new();
    parser
        .set_language(&lang)
        .map_err(|e| format!("Parser language error: {e}"))?;
    let tree = parser
        .parse(&src, None)
        .ok_or_else(|| format!("Failed to parse `{path}`"))?;

    let mut defs: Vec<Def> = Vec::new();
    let mut seen: HashSet<(String, String, usize)> = HashSet::new();
    collect_defs(tree.root_node(), &src, 0, max_depth, &mut defs, &mut seen);
    defs.sort_by_key(|a| a.line);
    defs.truncate(300);

    let mut out = String::new();
    for d in &defs {
        let sig = d.sig.trim();
        if sig.is_empty() {
            continue;
        }
        out.push_str(&format!(
            "{:>6}  {:<20} {}",
            format!("L{}", d.line),
            d.kind,
            sig.chars().take(120).collect::<String>(),
        ));
        out.push('\n');
    }

    let summary = format!(
        "Parsed `{}` - {} top-level declaration(s)",
        Path::new(path)
            .file_name()
            .map(|f| f.to_string_lossy())
            .unwrap_or_default(),
        defs.len()
    );
    Ok(ToolResult::ok(
        "view_file_structure",
        summary,
        Some(out),
        Some(json!({ "declarations": defs.len(), "maxDepth": max_depth, "path": path })),
    ))
}

fn is_definition(kind: &str) -> bool {
    matches!(
        kind,
        "function_declaration"
            | "class_declaration"
            | "abstract_class_declaration"
            | "method_definition"
            | "method_signature"
            | "interface_declaration"
            | "type_alias_declaration"
            | "enum_declaration"
            | "lexical_declaration"
            | "variable_declaration"
            | "function_signature"
            | "internal_module"
            | "external_module"
            | "ambient_declaration"
            | "construct_signature"
            | "index_signature"
            | "property_signature"
    )
}

fn is_declaration_parent(parent: Option<Node>) -> bool {
    parent
        .map(|p| {
            matches!(
                p.kind(),
                "program"
                    | "export_statement"
                    | "internal_module"
                    | "external_module"
                    | "class_declaration"
                    | "abstract_class_declaration"
                    | "interface_declaration"
                    | "enum_declaration"
                    | "ambient_declaration"
            )
        })
        .unwrap_or(false)
}

fn collect_defs(
    node: Node,
    src: &[u8],
    depth: usize,
    max_depth: usize,
    out: &mut Vec<Def>,
    seen: &mut HashSet<(String, String, usize)>,
) {
    let kind = node.kind().to_string();
    if depth <= max_depth && is_definition(&kind) && is_declaration_parent(node.parent()) {
        let name = node
            .child_by_field_name("name")
            .and_then(|n| n.utf8_text(src).ok())
            .unwrap_or("")
            .to_string();
        let sig = node
            .utf8_text(src)
            .unwrap_or("")
            .lines()
            .next()
            .unwrap_or("")
            .trim()
            .to_string();
        let start_line = node.start_position().row + 1;
        let end_line = node.end_position().row + 1;
        if seen.insert((kind.clone(), name.clone(), start_line)) {
            out.push(Def {
                kind,
                name,
                sig,
                line: start_line,
                end_line,
            });
        }
    }
    if depth >= max_depth {
        return;
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_defs(child, src, depth + 1, max_depth, out, seen);
    }
}

// ---------------------------------------------------------------------------
// read_file_range
// ---------------------------------------------------------------------------

const MAX_READ_LINES: u64 = 2000;

async fn read_file_range(path: &str, start_line: u64, end_line: u64) -> Result<ToolResult, String> {
    let bytes = tokio::fs::read(path)
        .await
        .map_err(|e| format!("Cannot read `{path}`: {e}"))?;
    let text = String::from_utf8_lossy(&bytes);
    let lines: Vec<&str> = text.lines().collect();
    let total = lines.len() as u64;

    if total == 0 {
        return Ok(ToolResult::ok(
            "read_file_range",
            format!("`{path}` is empty"),
            Some(String::new()),
            Some(json!({ "totalLines": 0 })),
        ));
    }

    let start = start_line.clamp(1, total);
    let mut end = end_line.clamp(start, total);
    let truncated = end - start + 1 > MAX_READ_LINES;
    if truncated {
        end = start + MAX_READ_LINES - 1;
    }

    let snippet = lines[(start - 1) as usize..end as usize].join("\n");
    let name = Path::new(path)
        .file_name()
        .map(|f| f.to_string_lossy())
        .unwrap_or_default();
    let summary = if truncated {
        format!("Read `{name}` lines {start}..={end} of {total} (truncated to {MAX_READ_LINES})")
    } else {
        format!("Read `{name}` lines {start}..={end} of {total}")
    };

    Ok(ToolResult::ok(
        "read_file_range",
        summary,
        Some(snippet),
        Some(
            json!({ "totalLines": total, "readFrom": start, "readTo": end, "truncated": truncated, "bytes": bytes.len() }),
        ),
    ))
}

// ---------------------------------------------------------------------------
// apply_file_diff
// ---------------------------------------------------------------------------

/// Parse a SEARCH/REPLACE block:
/// ```text
/// @@
/// SEARCH:
/// <old lines>
/// REPLACE:
/// <new lines>
/// ```
fn parse_search_replace(block: &str) -> Result<(String, String), String> {
    let mut b = block.trim();
    if let Some(rest) = b.strip_prefix("@@") {
        b = rest.trim_start();
    }
    let Some(rest) = b.strip_prefix("SEARCH:") else {
        return Err(
            "Diff block must be:\n\n@@\nSEARCH:\n<existing lines>\n\nREPLACE:\n<new lines>\n"
                .to_string(),
        );
    };
    b = rest.trim_start();

    let marker = find_replace_marker(b)
        .ok_or_else(|| "Diff block is missing the `REPLACE:` marker".to_string())?;
    let search = b[..marker].trim_end().to_string();
    let replace = b[marker + "REPLACE:".len()..].trim().to_string();
    Ok((search, replace))
}

fn find_replace_marker(text: &str) -> Option<usize> {
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        let content = line.trim_end_matches(['\r', '\n']).trim();
        if content == "REPLACE:" {
            return Some(offset);
        }
        offset += line.len();
    }
    None
}

/// Locate `search` inside `content` by anchoring on its longest line. Returns
/// the starting line index and the number of mismatching lines.
fn fuzzy_locate(content: &str, search: &str) -> Option<(usize, usize)> {
    let file_lines: Vec<&str> = content.lines().collect();
    let search_lines: Vec<&str> = search.lines().collect();
    if search_lines.is_empty() || search_lines.len() > file_lines.len() {
        return None;
    }
    let anchor_index = search_lines
        .iter()
        .enumerate()
        .max_by_key(|(_, l)| l.trim().chars().count())
        .map(|(i, _)| i)?;
    let anchor = search_lines[anchor_index].trim();
    if anchor.is_empty() {
        return None;
    }

    let mut best: Option<(usize, usize)> = None;
    for (fi, fl) in file_lines.iter().enumerate() {
        if fl.trim() != anchor {
            continue;
        }
        let start = fi as isize - anchor_index as isize;
        if start < 0 || start as usize + search_lines.len() > file_lines.len() {
            continue;
        }
        let start = start as usize;
        let region = &file_lines[start..start + search_lines.len()];
        let mismatches = region
            .iter()
            .zip(search_lines.iter())
            .filter(|(a, b)| a.trim() != b.trim())
            .count();
        if best.map_or(true, |(_, bm)| mismatches < bm) {
            best = Some((start, mismatches));
        }
        if mismatches == 0 {
            break;
        }
    }
    best
}

async fn apply_file_diff(app: &AppHandle, path: &str, diff: &str) -> Result<ToolResult, String> {
    let (search, replace) = parse_search_replace(diff)?;
    if search.trim().is_empty() {
        return Err(
            "SEARCH block is empty - include the exact existing lines to replace".to_string(),
        );
    }
    if replace.trim().is_empty() {
        return Err("REPLACE block is empty - include the new lines".to_string());
    }

    let content = tokio::fs::read_to_string(path)
        .await
        .map_err(|e| format!("Cannot read `{path}`: {e}"))?;
    let crlf = content.contains("\r\n");
    let norm = |s: &str| s.replace("\r\n", "\n");
    let content_n = norm(&content);
    let search_n = norm(search.trim_end());
    let replace_n = norm(replace.trim_end());

    let (new_content_n, strategy) = if let Some(pos) = content_n.find(&search_n) {
        let mut s = String::with_capacity(content_n.len() + replace_n.len());
        s.push_str(&content_n[..pos]);
        s.push_str(&replace_n);
        s.push_str(&content_n[pos + search_n.len()..]);
        (s, "exact".to_string())
    } else {
        let (start_idx, mismatches) = fuzzy_locate(&content_n, &search_n).ok_or_else(|| {
            format!(
                "SEARCH block not found in `{path}`. It may have been edited since you read it - re-read the file and retry with the current contents."
            )
        })?;
        if mismatches > 3 {
            return Err(format!(
                "SEARCH block match is too unreliable ({mismatches} line mismatches). Re-read the file and retry with the exact current contents."
            ));
        }
        let strategy = if mismatches == 0 {
            "line-anchored".to_string()
        } else {
            format!("fuzzy ({mismatches} line diff)")
        };

        let file_lines: Vec<&str> = content_n.lines().collect();
        let search_lines: Vec<&str> = search_n.lines().collect();
        let replace_lines: Vec<&str> = replace_n.lines().collect();
        let mut out =
            Vec::with_capacity(file_lines.len() - search_lines.len() + replace_lines.len() + 1);
        out.extend_from_slice(&file_lines[..start_idx]);
        out.extend_from_slice(&replace_lines);
        out.extend_from_slice(&file_lines[start_idx + search_lines.len()..]);
        (out.join("\n"), strategy)
    };

    let new_content = if crlf {
        new_content_n.replace('\n', "\r\n")
    } else {
        new_content_n
    };

    write_atomic(path, new_content.as_bytes()).await?;
    emit_file_changed(app, path, "diff", &content, &new_content);

    let removed = search_n.lines().count();
    let added = replace_n.lines().count();
    let stats = json!({
        "removedLines": removed,
        "addedLines": added,
        "strategy": strategy,
    });
    Ok(ToolResult::ok(
        "apply_file_diff",
        format!("Applied edit to `{path}` ({strategy}): {removed} lines removed, {added} added"),
        None,
        Some(stats),
    ))
}

async fn write_atomic(path: &str, bytes: &[u8]) -> Result<(), String> {
    let p = Path::new(path);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let tmp = PathBuf::from(format!(
        "{}.{}.{nanos}.agent-tmp",
        p.display(),
        std::process::id()
    ));
    tokio::fs::write(&tmp, bytes)
        .await
        .map_err(|e| format!("Failed to write temp file: {e}"))?;
    tokio::fs::rename(&tmp, p).await.map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("Failed to commit edit to `{path}`: {e}")
    })?;
    Ok(())
}

// ---------------------------------------------------------------------------
// execute_terminal_command
// ---------------------------------------------------------------------------

const MAX_CMD_OUTPUT: u64 = 200_000;

fn shell() -> &'static str {
    if cfg!(windows) {
        "cmd"
    } else {
        "sh"
    }
}

async fn execute_terminal_command(
    app: &AppHandle,
    state: &ToolState,
    interrupt: &CancellationToken,
    command: &str,
    timeout_secs: Option<u64>,
    cwd: Option<&str>,
) -> Result<ToolResult, String> {
    let timeout = std::time::Duration::from_secs(timeout_secs.unwrap_or(30).clamp(1, 300));
    let dir = match cwd {
        Some(c) => PathBuf::from(c),
        None => resolve_root(state, None)
            .await
            .unwrap_or_else(|_| std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))),
    };
    if !dir.is_dir() {
        return Err(format!("cwd does not exist: {}", dir.display()));
    }

    let mut cmd = tokio::process::Command::new(shell());
    if cfg!(windows) {
        cmd.arg("/C");
    } else {
        cmd.arg("-c");
    }
    cmd.arg(command)
        .current_dir(&dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    {
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("Failed to start command: {e}"))?;
    let pid = child.id();
    let stdout = child.stdout.take().expect("stdout piped");
    let stderr = child.stderr.take().expect("stderr piped");

    let task = async move {
        use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
        let app_for_output = app.clone();

        async fn pump<R>(
            app: &AppHandle,
            stream: &str,
            mut lines: tokio::io::Lines<BufReader<R>>,
            sink: &mut String,
            cap: usize,
        ) where
            R: AsyncRead + Unpin,
        {
            while let Ok(Some(line)) = lines.next_line().await {
                sink.push_str(&line);
                sink.push('\n');
                let _ = app.emit(
                    "agent://tool-output",
                    json!({
                        "tool": "execute_terminal_command",
                        "stream": stream,
                        "chunk": line,
                    }),
                );
                if sink.len() > cap {
                    break;
                }
            }
        }

        let mut so = String::new();
        let mut se = String::new();
        tokio::join!(
            pump(
                &app_for_output,
                "stdout",
                BufReader::new(stdout).lines(),
                &mut so,
                MAX_CMD_OUTPUT as usize
            ),
            pump(
                &app_for_output,
                "stderr",
                BufReader::new(stderr).lines(),
                &mut se,
                MAX_CMD_OUTPUT as usize
            ),
        );
        let status = child.wait().await;
        (status, so, se)
    };

    let started = Instant::now();

    enum CmdOutcome {
        Finished(std::io::Result<std::process::ExitStatus>, String, String),
        TimedOut,
    }

    // Race: command completion vs. timeout vs. the global circuit breaker.
    // Dropping `task` on a timeout/abort also drops the child (`kill_on_drop`),
    // so no orphaned process can survive an interruption.
    let outcome = tokio::select! {
        status = task => {
            let (status, so, se) = status;
            CmdOutcome::Finished(status, so, se)
        }
        _ = tokio::time::sleep(timeout) => CmdOutcome::TimedOut,
        _ = interrupt.clone().cancelled_owned() => {
            let _ = kill_tree(pid);
            let summary = super::interrupt::ABORT_REASON.to_string();
            return Ok(ToolResult::err(
                "execute_terminal_command",
                summary.clone(),
                summary,
            ));
        }
    };

    match outcome {
        CmdOutcome::TimedOut => {
            let _ = kill_tree(pid);
            let msg = format!(
                "Command timed out after {}s. If the command normally takes longer, retry with a larger `timeout_secs`.",
                timeout.as_secs()
            );
            Ok(ToolResult::err(
                "execute_terminal_command",
                msg.clone(),
                msg,
            ))
        }
        CmdOutcome::Finished(status, so, se) => {
            let status = match status {
                Ok(s) => s,
                Err(e) => return Err(format!("Command failed to run: {e}")),
            };
            let code = status.code();
            let success = status.success();
            let elapsed = started.elapsed().as_millis() as u64;
            let combined = if se.is_empty() {
                so.clone()
            } else {
                format!("{so}\n{se}")
            };
            let summary = if success {
                format!(
                    "Command succeeded (exit {}) in {elapsed}ms",
                    code.unwrap_or(0)
                )
            } else {
                format!(
                    "Command failed (exit {}) in {elapsed}ms",
                    code.unwrap_or(-1)
                )
            };
            Ok(ToolResult {
                success,
                tool: "execute_terminal_command".to_string(),
                summary,
                stdout: Some(combined),
                error: if success {
                    None
                } else {
                    Some(format!("exit code {}", code.unwrap_or(-1)))
                },
                stats: Some(json!({
                    "exitCode": code,
                    "cwd": dir.display().to_string(),
                    "truncatedOutput": so.len() >= MAX_CMD_OUTPUT as usize,
                })),
                duration_ms: elapsed,
            })
        }
    }
}

fn kill_tree(pid: Option<u32>) -> std::io::Result<()> {
    if let Some(pid) = pid {
        #[cfg(windows)]
        {
            let _ = std::process::Command::new("taskkill")
                .args(["/PID", &pid.to_string(), "/T", "/F"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()?;
        }
        #[cfg(not(windows))]
        {
            let _ = std::process::Command::new("kill")
                .args(["-9", &pid.to_string()])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// call_mcp_tool
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
async fn call_mcp_tool(
    app: &tauri::AppHandle,
    state: &ToolState,
    interrupt: &CancellationToken,
    server: Option<&str>,
    server_bin: Option<&str>,
    server_args: &[String],
    tool: &str,
    arguments: &Value,
    timeout_secs: Option<u64>,
) -> Result<ToolResult, String> {
    let timeout = std::time::Duration::from_secs(timeout_secs.unwrap_or(30).clamp(1, 300));

    // Resolve the connection target: a named catalog entry (preferred) or an
    // ad-hoc `bin + args` pair for one-off servers.
    let entry: Option<super::mcp::McpServerConfig> = match (server, server_bin) {
        (Some(name), _) => {
            let config_dir = crate::app_config_dir(app);
            let catalog = super::mcp::load_catalog(&config_dir)?;
            match catalog.into_iter().find(|c| c.name == name && c.enabled) {
                Some(c) => Some(c),
                None => {
                    return Err(format!(
                        "No enabled MCP server named `{name}` in the catalog; call list_mcp_servers to see available names"
                    ));
                }
            }
        }
        _ => None,
    };
    // Per-server allowed-tools filtering: an empty list allows everything;
    // entries with a trailing `*` act as prefix wildcards.
    if let Some(cfg) = &entry {
        if !cfg.tool_allowed(tool) {
            return Err(format!(
                "Tool `{tool}` is blocked by the allowed-tools filter on MCP server `{}` \
                 (allowed: {}); the user can adjust it in Settings.",
                cfg.name,
                if cfg.allowed_tools.is_empty() {
                    "everything".to_string()
                } else {
                    cfg.allowed_tools.join(", ")
                }
            ));
        }
    }
    let (bin, args, env): (String, Vec<String>, BTreeMap<String, String>) = match &entry {
        Some(c) => (c.bin.clone(), c.args.clone(), c.env.clone()),
        None => match server_bin {
            Some(b) => (b.to_string(), server_args.to_vec(), BTreeMap::new()),
            None => {
                return Err(
                    "Provide either `server` (a catalog name) or `serverBin` (an executable path)"
                        .to_string(),
                );
            }
        },
    };
    let key = format!("{bin} {}", args.join(" "));

    let handle = {
        let mut servers = state.mcp_servers.lock().await;
        if let Some(h) = servers.get(&key) {
            h.clone()
        } else {
            let h = std::sync::Arc::new(tokio::sync::Mutex::new(
                super::mcp::McpHandle::spawn(&bin, &args, &env).await?,
            ));
            servers.insert(key.clone(), h.clone());
            h
        }
    };

    let resp = {
        let mut guard = handle.lock().await;
        tokio::select! {
            r = guard.call_tool(tool, arguments) => r,
            _ = tokio::time::sleep(timeout) => {
                return Err(format!("MCP tool `{tool}` timed out after {}s", timeout.as_secs()));
            }
            _ = interrupt.clone().cancelled_owned() => {
                // Drop the orphaned server connection (stdin EOF) so nothing leaks.
                state.mcp_servers.lock().await.remove(&key);
                let summary = super::interrupt::ABORT_REASON.to_string();
                return Ok(ToolResult::err("call_mcp_tool", summary.clone(), summary));
            }
        }
    }?;

    let text = resp
        .get("text")
        .and_then(|t| t.as_str())
        .unwrap_or("")
        .to_string();

    Ok(ToolResult::ok(
        "call_mcp_tool",
        format!(
            "MCP `{tool}` returned {} character(s)",
            text.chars().count()
        ),
        Some(text),
        Some(json!({ "mcpServer": key, "tool": tool })),
    ))
}

// ---------------------------------------------------------------------------
// MCP server catalog management (list_mcp_servers / add / remove)
// ---------------------------------------------------------------------------

/// Show the user's configured MCP servers (name, command, enabled flag).
async fn list_mcp_servers(app: &tauri::AppHandle) -> Result<ToolResult, String> {
    let config_dir = crate::app_config_dir(app);
    let catalog = super::mcp::load_catalog(&config_dir)?;

    let mut lines = Vec::new();
    for c in &catalog {
        let status = if c.enabled { "enabled" } else { "disabled" };
        let filter = if c.allowed_tools.is_empty() {
            String::new()
        } else {
            format!(" [allowed tools: {}]", c.allowed_tools.join(", "))
        };
        lines.push(format!(
            "- {} [{}] {} {}{}",
            c.name,
            status,
            c.bin,
            c.args.join(" "),
            filter
        ));
    }
    if lines.is_empty() {
        lines.push(
            "(no MCP servers configured yet — add one with add_mcp_server, or the user can in Settings)".to_string(),
        );
    }
    let text = lines.join("\n");
    Ok(ToolResult::ok(
        "list_mcp_servers",
        format!("{} MCP server(s) configured", catalog.len()),
        Some(text.clone()),
        Some(json!({ "servers": catalog })),
    ))
}

/// Register an MCP server in the persisted global catalog. The entry is
/// callable by name from `call_mcp_tool` afterwards. Adding requires approval
/// (not in default_allow) because it wires an arbitrary executable into the
/// agent's toolset.
async fn add_mcp_server(
    app: &tauri::AppHandle,
    name: &str,
    bin: &str,
    args: &[String],
) -> Result<ToolResult, String> {
    let name = name.trim();
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
    {
        return Err(
            "Server `name` must be non-empty and contain only letters, digits, `-` or `_`"
                .to_string(),
        );
    }
    if bin.trim().is_empty() {
        return Err("`bin` must be a non-empty executable path or PATH command".to_string());
    }

    let config_dir = crate::app_config_dir(app);
    let mut catalog = super::mcp::load_catalog(&config_dir)?;
    if catalog.iter().any(|c| c.name == name) {
        return Err(format!(
            "A server named `{name}` already exists; remove it first with remove_mcp_server"
        ));
    }
    catalog.push(super::mcp::McpServerConfig {
        name: name.to_string(),
        bin: bin.trim().to_string(),
        args: args.to_vec(),
        env: BTreeMap::new(),
        allowed_tools: Vec::new(),
        enabled: true,
    });
    super::mcp::save_catalog(&config_dir, &catalog)?;
    drop(catalog);

    Ok(ToolResult::ok(
        "add_mcp_server",
        format!("MCP server `{name}` added to the catalog"),
        Some(format!(
            "Added `{name}` → {bin} {}\nThe server can now be called via call_mcp_tool with \"server\": \"{name}\".",
            args.join(" ")
        )),
        None,
    ))
}

/// Remove a server from the catalog and evict any cached connection to it.
async fn remove_mcp_server(
    app: &tauri::AppHandle,
    state: &ToolState,
    name: &str,
) -> Result<ToolResult, String> {
    let config_dir = crate::app_config_dir(app);
    let mut catalog = super::mcp::load_catalog(&config_dir)?;
    let pos = catalog
        .iter()
        .position(|c| c.name == name)
        .ok_or_else(|| format!("No MCP server named `{name}` in the catalog"))?;
    let removed = catalog.remove(pos);
    super::mcp::save_catalog(&config_dir, &catalog)?;
    drop(catalog);

    // Evict the live connection so a re-added server starts fresh.
    let key = format!("{} {}", removed.bin, removed.args.join(" "));
    state.mcp_servers.lock().await.remove(&key);

    Ok(ToolResult::ok(
        "remove_mcp_server",
        format!("MCP server `{name}` removed"),
        Some(format!("Removed `{}` → {}", removed.name, removed.bin)),
        None,
    ))
}

// ---------------------------------------------------------------------------
// RAG attachments (attach_file / search_attached_files / detach_file)
// ---------------------------------------------------------------------------

/// Chunk + embed a text file into the session attachment index.
async fn attach_file(state: &ToolState, path: &str) -> Result<ToolResult, String> {
    let p = PathBuf::from(path);
    let absolute = if p.is_absolute() {
        p
    } else {
        let ws = state.workspace.lock().await.clone().unwrap_or_default();
        ws.join(&p)
    };
    let text = std::fs::read_to_string(&absolute)
        .map_err(|e| format!("Failed to read `{}`: {e}", absolute.display()))?;
    let shown = absolute.to_string_lossy().into_owned();

    let file = {
        let mut rag = state.rag.lock().unwrap();
        rag.attach(&shown, &text)?
    };

    Ok(ToolResult::ok(
        "attach_file",
        format!(
            "Indexed `{}` ({} chunk(s), {} bytes)",
            shown, file.chunk_count, file.bytes
        ),
        Some(format!(
            "Attached `{}` — {} chunk(s) indexed. Use search_attached_files to query it.",
            shown, file.chunk_count
        )),
        Some(json!({ "path": shown, "chunks": file.chunk_count, "bytes": file.bytes })),
    ))
}

/// Semantic search over all attached files; returns the top chunks with their
/// source path + character offset.
async fn search_attached_files(
    state: &ToolState,
    query: &str,
    top_k: Option<usize>,
) -> Result<ToolResult, String> {
    let rag = state.rag.lock().unwrap();
    if rag.list().is_empty() {
        return Err("No files attached yet; call attach_file first".to_string());
    }
    let hits = rag.search(query, top_k.unwrap_or(5));
    drop(rag);

    if hits.is_empty() {
        return Ok(ToolResult::ok(
            "search_attached_files",
            "No matching chunks".to_string(),
            Some("(no matches)".to_string()),
            None,
        ));
    }

    let mut lines = Vec::new();
    for (path, offset, score, text) in &hits {
        lines.push(format!(
            "--- {} @ char {offset} (score {:.3})\n{}",
            path, score, text
        ));
    }
    Ok(ToolResult::ok(
        "search_attached_files",
        format!("{} matching chunk(s)", hits.len()),
        Some(lines.join("\n\n")),
        None,
    ))
}

/// Remove a file from the attachment index.
async fn detach_file(state: &ToolState, path: &str) -> Result<ToolResult, String> {
    let removed = {
        let mut rag = state.rag.lock().unwrap();
        rag.detach(path)
    };
    if removed {
        Ok(ToolResult::ok(
            "detach_file",
            format!("Detached `{path}`"),
            Some(format!("`{path}` removed from the attachment index.")),
            None,
        ))
    } else {
        Err(format!("`{path}` is not attached"))
    }
}

// ---------------------------------------------------------------------------
// Voice transcription (transcribe_audio tool + UI dictation share this)
// ---------------------------------------------------------------------------

/// Locate an OpenAI-whisper CLI on PATH (`whisper`, or `whisper.exe`).
pub fn transcribe_discover() -> Option<PathBuf> {
    let exe_names: &[&str] = if cfg!(windows) {
        &["whisper.exe", "whisper"]
    } else {
        &["whisper"]
    };
    let path_var = std::env::var("PATH").unwrap_or_default();
    for dir in std::env::split_paths(&path_var) {
        for name in exe_names {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// Transcribe an audio/video file with the local whisper CLI into the
/// session scratchpad. Requires the `openai-whisper` pip package (`whisper`
/// on PATH); ffmpeg must also be installed for compressed formats.
pub async fn transcribe_file(state: &ToolState, path: &Path) -> Result<String, String> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        let ws = state.workspace.lock().await.clone().unwrap_or_default();
        ws.join(path)
    };
    if !absolute.is_file() {
        return Err(format!("Not a file: {}", absolute.display()));
    }

    let cli = transcribe_discover()
        .ok_or("NO_TRANSCRIBER: install openai-whisper (pip install -U openai-whisper) and ffmpeg, then retry")?;

    let out_dir =
        super::session_scratchpad(state.session_id.load(std::sync::atomic::Ordering::Relaxed))
            .join("transcripts");
    std::fs::create_dir_all(&out_dir).map_err(|e| format!("mkdir failed: {e}"))?;
    let out_str = out_dir.to_string_lossy().into_owned();

    let mut cmd = tokio::process::Command::new(&cli);
    cmd.arg(absolute.as_os_str())
        .arg("--model")
        .arg("base")
        .arg("--output_format")
        .arg("txt")
        .arg("--output_dir")
        .arg(&out_str)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    {
        // tokio's Command exposes creation_flags natively on Windows.
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let output = tokio::time::timeout(std::time::Duration::from_secs(600), cmd.output())
        .await
        .map_err(|_| "Transcription timed out after 600s".to_string())?
        .map_err(|e| format!("Failed to run whisper: {e}"))?;
    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "whisper failed (exit {:?}): {}",
            output.status.code(),
            err.chars().take(600).collect::<String>()
        ));
    }

    // Newest .txt in the output dir is our transcript (whisper names it after
    // the input stem).
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    for entry in std::fs::read_dir(&out_dir)
        .map_err(|e| e.to_string())?
        .flatten()
    {
        let p = entry.path();
        if p.extension().map(|e| e == "txt").unwrap_or(false) {
            let modified = entry
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(std::time::UNIX_EPOCH);
            if best.as_ref().map(|(t, _)| modified > *t).unwrap_or(true) {
                best = Some((modified, p));
            }
        }
    }
    let (_, transcript_path) = best.ok_or("whisper produced no transcript")?;
    std::fs::read_to_string(&transcript_path)
        .map(|t| t.trim().to_string())
        .map_err(|e| format!("Failed to read transcript: {e}"))
}

/// The `transcribe_audio` agent tool.
async fn transcribe_audio_tool(
    state: &ToolState,
    path: &str,
    _language: Option<&str>,
) -> Result<ToolResult, String> {
    let shown = path.to_string();
    let text = transcribe_file(state, Path::new(path)).await?;
    Ok(ToolResult::ok(
        "transcribe_audio",
        format!(
            "Transcribed `{}` ({} characters)",
            shown,
            text.chars().count()
        ),
        Some(text),
        None,
    ))
}

// ---------------------------------------------------------------------------
// write_file
// ---------------------------------------------------------------------------

/// Write a file atomically and notify the frontend for editor sync.
async fn write_file(app: &AppHandle, path: &str, content: &str) -> Result<ToolResult, String> {
    let before = tokio::fs::read_to_string(path).await.unwrap_or_default();
    write_atomic(path, content.as_bytes()).await?;
    emit_file_changed(app, path, "write", &before, content);
    let bytes = content.len();
    Ok(ToolResult::ok(
        "write_file",
        format!("Wrote `{path}` ({bytes} bytes)"),
        None,
        Some(json!({ "path": path, "bytes": bytes })),
    ))
}

// ---------------------------------------------------------------------------
// create_skill — self-development: persist a reusable skill the model learned
// ---------------------------------------------------------------------------

fn sanitize_skill_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let cleaned = cleaned.trim_matches('-').to_string();
    if cleaned.is_empty() {
        "skill".into()
    } else {
        cleaned
    }
}

async fn create_skill(
    app: &AppHandle,
    state: &ToolState,
    name: &str,
    description: Option<&str>,
    content: &str,
) -> Result<ToolResult, String> {
    let workspace = resolve_root(state, None).await?;
    let dir = workspace.join(".ai").join("skills");
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| format!("Cannot create skills dir: {e}"))?;

    let slug = sanitize_skill_name(name);
    let path = dir.join(format!("{slug}.md"));
    let desc = description.unwrap_or(name).trim();
    let body = content.trim();

    let frontmatter = format!(
        "---\nname: {name}\ndescription: {desc}\ncreated: {created}\n---\n\n",
        created = now_ms()
    );
    let full = format!("{frontmatter}{body}\n");

    tokio::fs::write(&path, full.as_bytes())
        .await
        .map_err(|e| format!("Cannot write skill `{name}`: {e}"))?;

    let _ = app.emit(
        "agent://skills-changed",
        json!({ "name": name, "path": path.to_string_lossy() }),
    );
    let bytes = full.len();
    Ok(ToolResult::ok(
        "create_skill",
        format!("Learned new skill `{name}` saved to `.ai/skills/{slug}.md` ({bytes} bytes)"),
        None,
        Some(json!({
            "name": name,
            "path": path.to_string_lossy(),
            "bytes": bytes,
        })),
    ))
}

// read_skill — load the full text of any available skill on demand
// ---------------------------------------------------------------------------

/// Return the full, untruncated content of a skill by name. The model calls
/// this when the pinned `## Skill instructions` section clipped a skill to
/// save context, or when it needs the complete procedure before acting.
async fn read_skill(state: &ToolState, name: &str) -> Result<ToolResult, String> {
    let skill = state.knowledge.get_skill(name).ok_or_else(|| {
        let available = state.knowledge.skill_names();
        if available.is_empty() {
            "No skills are available. Create one with create_skill or add `.ai/skills/*.md` files to the workspace.".to_string()
        } else {
            format!(
                "No skill named `{name}`. Available skills: {}.",
                available.join(", ")
            )
        }
    })?;
    let mut out = format!("# Skill: {}\n", skill.name);
    if !skill.description.is_empty() {
        out.push_str(&format!("Description: {}\n", skill.description));
    }
    out.push_str(&format!("Source: {}\n\n", skill.source));
    out.push_str(skill.content.trim());
    out.push('\n');
    Ok(ToolResult::ok(
        "read_skill",
        format!(
            "Loaded skill `{}` ({} chars)",
            skill.name,
            skill.content.len()
        ),
        Some(out),
        Some(json!({
            "name": skill.name,
            "active": skill.active,
        })),
    ))
}

// ---------------------------------------------------------------------------
// run_tests
// ---------------------------------------------------------------------------

/// Run the project's test suite. Auto-detects `npm test` / `cargo test` from
/// the workspace when no explicit command is supplied.
async fn run_tests(
    app: &AppHandle,
    state: &ToolState,
    interrupt: &CancellationToken,
    command: Option<&str>,
) -> Result<ToolResult, String> {
    let dir = resolve_root(state, None).await?;
    let command = match command {
        Some(c) if !c.trim().is_empty() => c.to_string(),
        _ => detect_test_command(&dir)?,
    };
    let result = execute_terminal_command(
        app,
        state,
        interrupt,
        &command,
        Some(180),
        Some(dir.to_str().unwrap_or(".")),
    )
    .await?;
    let ok = result.success;
    let summary = if ok {
        format!("Tests passed: `{command}`")
    } else {
        format!("Tests failed: `{command}` (see output)")
    };
    Ok(ToolResult {
        success: ok,
        tool: "run_tests".into(),
        summary,
        stdout: result.stdout,
        error: if ok {
            None
        } else {
            Some("Test suite reported failures".into())
        },
        stats: result.stats,
        duration_ms: result.duration_ms,
    })
}

fn detect_test_command(dir: &Path) -> Result<String, String> {
    if dir.join("package.json").is_file() {
        Ok("npm test".into())
    } else if dir.join("Cargo.toml").is_file() {
        Ok("cargo test".into())
    } else if dir.join("Makefile").is_file() {
        Ok("make test".into())
    } else if dir.join("pom.xml").is_file() {
        Ok("mvn test".into())
    } else if dir.join("go.mod").is_file() {
        Ok("go test ./...".into())
    } else if dir.join("pyproject.toml").is_file() {
        Ok("pytest".into())
    } else {
        Err("No test command detected (add one via the `command` param, e.g. `npm test`)".into())
    }
}

// ---------------------------------------------------------------------------
// git tools
// ---------------------------------------------------------------------------

const GIT_TIMEOUT: u64 = 60;

async fn git_capture(
    state: &ToolState,
    interrupt: &CancellationToken,
    args: &[&str],
    path_filter: Option<&str>,
) -> Result<ToolResult, String> {
    let dir = resolve_root(state, None).await?;
    let mut full_args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    if let Some(p) = path_filter {
        if !p.is_empty() {
            full_args.push("--".to_string());
            full_args.push(p.to_string());
        }
    }

    let mut cmd = tokio::process::Command::new("git");
    cmd.args(&full_args)
        .current_dir(&dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    {
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let mut child = cmd.spawn().map_err(|e| {
        format!(
            "Failed to run `git {}`: {e} (is git on PATH?)",
            full_args.join(" ")
        )
    })?;
    let pid = child.id();
    let stdout = child.stdout.take().expect("stdout piped");
    let stderr = child.stderr.take().expect("stderr piped");

    let task = async move {
        let mut so = String::new();
        let mut se = String::new();
        {
            use tokio::io::AsyncReadExt;
            let mut r = tokio::io::BufReader::new(stdout).take(500_000);
            let _ = r.read_to_string(&mut so).await;
        }
        {
            use tokio::io::AsyncReadExt;
            let mut r = tokio::io::BufReader::new(stderr).take(200_000);
            let _ = r.read_to_string(&mut se).await;
        }
        let status = child.wait().await;
        (status, so, se)
    };

    enum GitOutcome {
        Finished(std::io::Result<std::process::ExitStatus>, String, String),
        TimedOut,
    }

    let outcome = tokio::select! {
        status = task => {
            let (status, so, se) = status;
            GitOutcome::Finished(status, so, se)
        }
        _ = tokio::time::sleep(Duration::from_secs(GIT_TIMEOUT)) => GitOutcome::TimedOut,
        _ = interrupt.clone().cancelled_owned() => {
            let _ = kill_tree(pid);
            return Err(super::interrupt::ABORT_REASON.to_string());
        }
    };

    let (status, so, se) = match outcome {
        GitOutcome::Finished(status, so, se) => {
            let status = match status {
                Ok(s) => s,
                Err(e) => return Err(format!("git failed to run: {e}")),
            };
            (status, so, se)
        }
        GitOutcome::TimedOut => {
            let _ = kill_tree(pid);
            return Err("git command timed out".into());
        }
    };

    let success = status.success();
    let combined = if se.is_empty() {
        so.clone()
    } else {
        format!("{so}\n{se}")
    };
    let cmd_str = format!("git {}", full_args.join(" "));
    let summary = format!(
        "`{cmd_str}` {} (exit {})",
        if success { "succeeded" } else { "failed" },
        status.code().unwrap_or(-1)
    );
    Ok(ToolResult {
        success,
        tool: "git".into(),
        summary,
        stdout: Some(combined),
        error: if success {
            None
        } else {
            Some(format!("exit code {}", status.code().unwrap_or(-1)))
        },
        stats: Some(json!({ "args": full_args })),
        duration_ms: 0,
    })
}

async fn git_diff(
    state: &ToolState,
    interrupt: &CancellationToken,
    path: Option<&str>,
) -> Result<ToolResult, String> {
    let stat = git_capture(state, interrupt, &["diff", "--stat"], None).await?;
    let mut body = git_capture(state, interrupt, &["diff"], path).await?;
    let mut combined = stat.stdout.unwrap_or_default();
    if let Some(b) = body.stdout.take() {
        combined.push_str("\n\n");
        combined.push_str(&b);
    }
    body.success = body.success && stat.success;
    body.stdout = Some(combined);
    body.summary = format!(
        "git diff {}",
        if path.is_some() {
            "for the requested path"
        } else {
            "(full)"
        }
    );
    Ok(body)
}

async fn git_commit(
    state: &ToolState,
    interrupt: &CancellationToken,
    message: &str,
) -> Result<ToolResult, String> {
    if message.trim().is_empty() {
        return Err("git_commit needs a non-empty message".into());
    }
    git_capture(state, interrupt, &["add", "-A"], None).await?;
    git_capture(state, interrupt, &["commit", "-m", message], None).await
}

/// Save a checkpoint: a real commit tagged with a `checkpoint:` prefix so
/// `git_revert` can find it later.
pub async fn git_checkpoint(
    state: &ToolState,
    interrupt: &CancellationToken,
    message: Option<&str>,
) -> Result<ToolResult, String> {
    let msg = match message {
        Some(m) if !m.trim().is_empty() => m.to_string(),
        _ => format!("checkpoint: {}", now_ms()),
    };
    let msg = if msg.starts_with("checkpoint:") {
        msg
    } else {
        format!("checkpoint: {msg}")
    };
    let commit = git_capture(state, interrupt, &["add", "-A"], None).await?;
    if !commit.success {
        return Ok(commit);
    }
    git_capture(state, interrupt, &["commit", "-m", &msg], None).await
}

/// List checkpoint commits (newest first): hash + subject + ISO-ish timestamp.
/// Returns `Ok(Vec::new())` if the workspace isn't a git repo or has none.
pub async fn git_checkpoints(
    state: &ToolState,
    interrupt: &CancellationToken,
) -> Result<Vec<serde_json::Value>, String> {
    let res = git_capture(
        state,
        interrupt,
        &[
            "log",
            "--grep=^checkpoint:",
            "--format=%H%x1f%s%x1f%ct",
            "-n",
            "20",
        ],
        None,
    )
    .await?;
    if !res.success {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    if let Some(stdout) = res.stdout {
        for line in stdout.lines() {
            let mut parts = line.splitn(3, '\x1f');
            let (Some(hash), Some(subject), Some(ts)) = (parts.next(), parts.next(), parts.next())
            else {
                continue;
            };
            let time = ts
                .trim()
                .parse::<i64>()
                .map(|s| {
                    use std::time::{SystemTime, UNIX_EPOCH};
                    let secs = if s >= 0 { s as u64 } else { 0 };
                    let d = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(secs);
                    let elapsed = d.duration_since(UNIX_EPOCH).unwrap_or_default();
                    let since = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default();
                    let ago = since.saturating_sub(elapsed).as_secs();
                    if ago < 60 {
                        "just now".to_string()
                    } else if ago < 3600 {
                        format!("{}m ago", ago / 60)
                    } else if ago < 86400 {
                        format!("{}h ago", ago / 3600)
                    } else {
                        format!("{}d ago", ago / 86400)
                    }
                })
                .unwrap_or_default();
            out.push(json!({
                "hash": hash,
                "subject": subject,
                "relative": time,
            }));
        }
    }
    Ok(out)
}

/// Revert to a checkpoint commit (or the most recent `checkpoint:` one). Uses
/// `reset --hard`; gated by policy `ask`, so a human always approves it.
pub async fn git_revert(
    state: &ToolState,
    interrupt: &CancellationToken,
    commit: Option<&str>,
) -> Result<ToolResult, String> {
    let target = match commit {
        Some(c) if !c.trim().is_empty() => c.to_string(),
        _ => {
            let head = git_capture(
                state,
                interrupt,
                &["log", "--grep=^checkpoint:", "--format=%H", "-1"],
                None,
            )
            .await?;
            match head.stdout.and_then(|s| s.split_whitespace().next().map(str::to_string)) {
                Some(h) if !h.is_empty() => h,
                _ => return Err("No checkpoint commit found to revert to. Create one with git_checkpoint first.".into()),
            }
        }
    };
    git_capture(state, interrupt, &["reset", "--hard", &target], None).await
}

// ---------------------------------------------------------------------------
// plan tools
// ---------------------------------------------------------------------------

async fn create_plan(
    state: &ToolState,
    title: &str,
    goal: &str,
    items: &[String],
) -> Result<ToolResult, String> {
    let workspace = resolve_root(state, None).await?;
    let plan = plan::new_plan(title, goal, items.to_vec());
    plan.save(&workspace)?;
    // Cache the active plan in ToolState so subsequent tools/loops see it.
    *state.plan.lock().unwrap() = Some(plan.clone());
    let md = plan.render_markdown();
    Ok(ToolResult::ok(
        "create_plan",
        format!(
            "Plan `{}` created with {} items. See `.ai/plan.md`.",
            plan.title,
            plan.items.len()
        ),
        Some(md),
        None,
    ))
}

async fn read_plan(state: &ToolState) -> Result<ToolResult, String> {
    let workspace = resolve_root(state, None).await?;
    let plan = {
        let guard = state.plan.lock().unwrap();
        match guard.as_ref() {
            Some(p) => p.clone(),
            None => plan::PlanState::load(&workspace)
                .ok_or_else(|| "No plan found. Call `create_plan` first.".to_string())?,
        }
    };
    let md = plan.render_markdown();
    Ok(ToolResult::ok(
        "read_plan",
        format!(
            "Plan `{}` — {}/{} items completed.",
            plan.title,
            plan.items
                .iter()
                .filter(|i| i.status == plan::PlanStatus::Completed)
                .count(),
            plan.items.len()
        ),
        Some(md),
        None,
    ))
}

async fn update_plan(
    state: &ToolState,
    item: usize,
    status: &str,
    details: Option<&str>,
) -> Result<ToolResult, String> {
    let workspace = resolve_root(state, None).await?;
    let new_status = plan::PlanStatus::from_label(status).ok_or_else(|| {
        format!("Unknown status `{status}`. Use: not_started, in_progress, completed, terminal.")
    })?;
    let mut plan = {
        let guard = state.plan.lock().unwrap();
        match guard.as_ref() {
            Some(p) => p.clone(),
            None => plan::PlanState::load(&workspace)
                .ok_or_else(|| "No plan found. Call `create_plan` first.".to_string())?,
        }
    };
    let total_items = plan.items.len();
    let plan_item = plan
        .items
        .get_mut(item - 1)
        .ok_or_else(|| format!("Plan item #{item} not found (plan has {total_items} items)."))?;
    let title = plan_item.title.clone();
    plan_item.status = new_status;
    if let Some(d) = details {
        if !plan_item.details.is_empty() {
            plan_item.details.push_str(" — ");
        }
        plan_item.details.push_str(d);
    }
    plan.updated_at = now_ms();
    plan.save(&workspace)?;
    *state.plan.lock().unwrap() = Some(plan.clone());
    Ok(ToolResult::ok(
        "update_plan",
        format!(
            "Updated plan item #{item} `{title}` → {}.",
            new_status.label()
        ),
        None,
        None,
    ))
}

// ---------------------------------------------------------------------------
// Bionic §3.2 filesystem completion: list_dir / read_file_chars /
// create_folder / copy / move / delete / get_scratchpad_folder
// ---------------------------------------------------------------------------

/// Default + max characters returned by `read_file_chars` (context protection).
const DEFAULT_READ_CHARS: usize = 4_000;
const MAX_READ_CHARS: usize = 24_000;
/// `create_folder` refuses paths deeper than this many segments (Bionic §3.2).
const MAX_FOLDER_DEPTH: usize = 50;
/// `list_dir` result cap.
const MAX_LIST_ENTRIES: usize = 2_000;

/// Resolve `path` against the workspace root when relative; pass absolute
/// paths through unchanged.
fn abs_from(root: &Path, path: &str) -> Result<PathBuf, String> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err("Path must not be empty.".into());
    }
    let p = PathBuf::from(trimmed);
    if p.is_absolute() {
        Ok(p)
    } else {
        Ok(root.join(p))
    }
}

/// UTF-8 character-offset slice used by `read_file_chars`. Returns the slice
/// plus whether more content follows.
fn char_slice(text: &str, offset: usize, limit: usize) -> (String, bool) {
    let total = text.chars().count();
    let limit = limit.clamp(1, MAX_READ_CHARS);
    let start = offset.min(total);
    let end = (start + limit).min(total);
    (
        text.chars().skip(start).take(end - start).collect(),
        end < total,
    )
}

/// Segment-count guard for `create_folder`.
fn folder_depth_ok(root: &Path, target: &Path) -> bool {
    let segs = target
        .strip_prefix(root)
        .map(|rel| rel.components().count())
        .unwrap_or_else(|_| target.components().count());
    segs <= MAX_FOLDER_DEPTH
}

async fn list_dir(state: &ToolState, path: Option<&str>) -> Result<ToolResult, String> {
    let root = resolve_root(state, None).await?;
    let target = match path.map(str::trim).filter(|p| !p.is_empty()) {
        Some(p) => abs_from(&root, p)?,
        None => root.clone(),
    };
    if !target.is_dir() {
        return Err(format!("Not a directory: {}", target.display()));
    }
    let mut rd = tokio::fs::read_dir(&target)
        .await
        .map_err(|e| format!("Cannot read directory {}: {e}", target.display()))?;
    let mut dirs: Vec<String> = Vec::new();
    let mut files: Vec<(String, u64)> = Vec::new();
    loop {
        let entry = match rd.next_entry().await {
            Ok(Some(e)) => e,
            Ok(None) => break,
            Err(e) => return Err(format!("Cannot list {}: {e}", target.display())),
        };
        let name = entry.file_name().to_string_lossy().to_string();
        let is_dir = entry.metadata().await.map(|m| m.is_dir()).unwrap_or(false);
        if is_dir {
            dirs.push(name);
        } else {
            let size = entry.metadata().await.map(|m| m.len()).unwrap_or(0);
            files.push((name, size));
        }
    }
    dirs.sort_by_key(|a| a.to_lowercase());
    files.sort_by_key(|a| a.0.to_lowercase());

    let mut lines: Vec<String> = Vec::new();
    for d in &dirs {
        if lines.len() >= MAX_LIST_ENTRIES {
            break;
        }
        lines.push(format!("{d}/"));
    }
    for (f, size) in &files {
        if lines.len() >= MAX_LIST_ENTRIES {
            lines.push(format!("… truncated at {MAX_LIST_ENTRIES} entries"));
            break;
        }
        lines.push(format!("{f} ({size} bytes)"));
    }
    let total = dirs.len() + files.len();
    let out = if lines.is_empty() {
        "(empty directory)".to_string()
    } else {
        lines.join("\n")
    };
    Ok(ToolResult::ok(
        "list_dir",
        format!("Listed {total} entries in `{}`", target.display()),
        Some(out),
        Some(json!({
            "path": target.to_string_lossy(),
            "dirs": dirs.len(),
            "files": files.len(),
            "truncated": total > MAX_LIST_ENTRIES,
        })),
    ))
}

async fn read_file_chars(path: &str, offset: usize, limit: usize) -> Result<ToolResult, String> {
    let text = tokio::fs::read_to_string(path)
        .await
        .map_err(|e| format!("Cannot read `{path}` as UTF-8 text: {e}"))?;
    let total = text.chars().count();
    let (slice, has_more) = char_slice(&text, offset, limit);
    let mut out = slice;
    if out.is_empty() {
        out.push_str("(no content in range)");
    }
    if has_more {
        let shown_end = (offset + out.chars().count()).min(total);
        let next_offset = (offset + limit.clamp(1, MAX_READ_CHARS)).min(total);
        out.push_str(&format!(
            "\n\n[Showing chars {offset}..{shown_end} of {total}. Call read_file_chars with offset={next_offset} to continue.]"
        ));
    } else {
        out.push_str("\n\n<EOF>");
    }
    Ok(ToolResult::ok(
        "read_file_chars",
        format!(
            "Read {} chars of `{path}` from offset {offset}",
            out.chars().count()
        ),
        Some(out),
        Some(json!({ "path": path, "offset": offset, "totalChars": total, "hasMore": has_more })),
    ))
}

async fn create_folder(state: &ToolState, path: &str) -> Result<ToolResult, String> {
    let root = resolve_root(state, None).await?;
    let target = abs_from(&root, path)?;
    if !folder_depth_ok(&root, &target) {
        return Err(format!(
            "Refusing to create `{path}`: exceeds the {MAX_FOLDER_DEPTH}-segment depth cap."
        ));
    }
    tokio::fs::create_dir_all(&target)
        .await
        .map_err(|e| format!("Cannot create folder {}: {e}", target.display()))?;
    Ok(ToolResult::ok(
        "create_folder",
        format!("Created folder `{}`", target.display()),
        None,
        Some(json!({ "path": target.to_string_lossy() })),
    ))
}

/// Recursively copy `from` onto `to`; returns total bytes copied.
fn copy_recursive(from: &Path, to: &Path) -> Result<u64, String> {
    if from.is_dir() {
        std::fs::create_dir_all(to).map_err(|e| format!("Cannot create {}: {e}", to.display()))?;
        let mut total = 0u64;
        let entries =
            std::fs::read_dir(from).map_err(|e| format!("Cannot read {}: {e}", from.display()))?;
        for entry in entries {
            let entry = entry.map_err(|e| format!("Cannot read {}: {e}", from.display()))?;
            total += copy_recursive(&entry.path(), &to.join(entry.file_name()))?;
        }
        Ok(total)
    } else {
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Cannot create {}: {e}", parent.display()))?;
        }
        std::fs::copy(from, to)
            .map_err(|e| format!("Cannot copy {} → {}: {e}", from.display(), to.display()))
    }
}

/// Pre-remove an existing destination when `can_overwrite` was granted.
fn clear_destination(to: &Path) -> Result<(), String> {
    if !to.exists() {
        return Ok(());
    }
    if to.is_dir() {
        std::fs::remove_dir_all(to).map_err(|e| format!("Cannot replace {}: {e}", to.display()))
    } else {
        std::fs::remove_file(to).map_err(|e| format!("Cannot replace {}: {e}", to.display()))
    }
}

async fn copy_file_or_folder(
    state: &ToolState,
    src: &str,
    dst: &str,
    can_overwrite: bool,
) -> Result<ToolResult, String> {
    let root = resolve_root(state, None).await?;
    let from = abs_from(&root, src)?;
    let to = abs_from(&root, dst)?;
    if !from.exists() {
        return Err(format!("Source does not exist: {}", from.display()));
    }
    if to.exists() && !can_overwrite {
        return Err(format!(
            "Destination already exists: {} (pass canOverwrite=true to replace it)",
            to.display()
        ));
    }
    let (f, t) = (from.clone(), to.clone());
    let bytes = tokio::task::spawn_blocking(move || -> Result<u64, String> {
        if can_overwrite {
            clear_destination(&t)?;
        }
        copy_recursive(&f, &t)
    })
    .await
    .map_err(|e| format!("Copy task failed: {e}"))??;
    Ok(ToolResult::ok(
        "copy_file_or_folder",
        format!(
            "Copied `{}` → `{}` ({bytes} bytes)",
            from.display(),
            to.display()
        ),
        None,
        Some(json!({
            "src": from.to_string_lossy(),
            "dst": to.to_string_lossy(),
            "bytes": bytes,
        })),
    ))
}

async fn move_file_or_folder(
    state: &ToolState,
    src: &str,
    dst: &str,
    can_overwrite: bool,
) -> Result<ToolResult, String> {
    let root = resolve_root(state, None).await?;
    let from = abs_from(&root, src)?;
    let to = abs_from(&root, dst)?;
    if !from.exists() {
        return Err(format!("Source does not exist: {}", from.display()));
    }
    if to.exists() && !can_overwrite {
        return Err(format!(
            "Destination already exists: {} (pass canOverwrite=true to replace it)",
            to.display()
        ));
    }
    let (f, t) = (from.clone(), to.clone());
    let bytes = tokio::task::spawn_blocking(move || -> Result<u64, String> {
        if can_overwrite {
            clear_destination(&t)?;
        }
        // Fast path: same-volume rename moves directories atomically.
        if std::fs::rename(&f, &t).is_ok() {
            return Ok(0u64);
        }
        // Fallback: cross-device move = copy + hard-delete the source.
        let copied = copy_recursive(&f, &t)?;
        if f.is_dir() {
            std::fs::remove_dir_all(&f)
                .map_err(|e| format!("Copied but cannot remove source {}: {e}", f.display()))?;
        } else {
            std::fs::remove_file(&f)
                .map_err(|e| format!("Copied but cannot remove source {}: {e}", f.display()))?;
        }
        Ok(copied)
    })
    .await
    .map_err(|e| format!("Move task failed: {e}"))??;
    let detail = if bytes == 0 {
        "renamed"
    } else {
        "copied across volumes"
    };
    Ok(ToolResult::ok(
        "move_file_or_folder",
        format!("Moved `{}` → `{}` ({detail})", from.display(), to.display()),
        None,
        Some(json!({
            "src": from.to_string_lossy(),
            "dst": to.to_string_lossy(),
            "bytes": bytes,
        })),
    ))
}

async fn delete_file_or_folder(state: &ToolState, path: &str) -> Result<ToolResult, String> {
    let root = resolve_root(state, None).await?;
    let target = abs_from(&root, path)?;
    if !target.exists() {
        return Err(format!("Path does not exist: {}", target.display()));
    }
    // Compare canonicalized paths so ".", trailing dots or symlinks cannot
    // disguise the workspace root (or its `.ai` state folder) as a deletable
    // target.
    let canon_target = target
        .canonicalize()
        .map_err(|e| format!("Cannot resolve {}: {e}", target.display()))?;
    let canon_root = root.canonicalize().unwrap_or_else(|_| root.clone());
    if canon_target == canon_root {
        return Err("Refusing to delete the workspace root itself.".into());
    }
    let dot_ai = canon_root.join(".ai");
    if canon_target == dot_ai.canonicalize().unwrap_or(dot_ai) {
        return Err(
            "Refusing to delete the workspace `.ai` folder (policy/audit/skills live there)."
                .into(),
        );
    }
    let t = target.clone();
    tokio::task::spawn_blocking(move || trash::delete(&t))
        .await
        .map_err(|e| format!("Delete task failed: {e}"))?
        .map_err(|e| format!("Cannot move {} to the OS Trash: {e}", target.display()))?;
    Ok(ToolResult::ok(
        "delete_file_or_folder",
        format!("Moved `{}` to the OS Trash", target.display()),
        None,
        Some(json!({ "path": target.to_string_lossy(), "trashed": true })),
    ))
}

async fn get_scratchpad_folder(state: &ToolState) -> Result<ToolResult, String> {
    let session = state.session_id.load(std::sync::atomic::Ordering::SeqCst);
    let dir = super::session_scratchpad(session);
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| format!("Cannot create scratchpad {}: {e}", dir.display()))?;
    let p = dir.to_string_lossy().to_string();
    Ok(ToolResult::ok(
        "get_scratchpad_folder",
        format!("Scratchpad ready at `{p}`"),
        Some(p.clone()),
        Some(json!({ "path": p, "sessionId": session })),
    ))
}

// ---------------------------------------------------------------------------
// Bionic §3.2 PLANNING: set_todo_list / get_todo_list / mark_todo_item_done
// ---------------------------------------------------------------------------

/// Load the active todo list from the workspace cache or disk.
async fn load_todos(state: &ToolState) -> Result<todo::TodoList, String> {
    let workspace = resolve_root(state, None).await?;
    Ok(todo::TodoList::load(&workspace).unwrap_or_default())
}

/// Number of still-open todo items in the current workspace (0 when none/no
/// workspace). Used by the orchestrator's finish-guard.
pub async fn open_todo_count(state: &ToolState) -> Result<usize, String> {
    Ok(load_todos(state).await?.open_count())
}

/// Persist + broadcast a todo list mutation.
fn save_and_emit_todos(
    app: &AppHandle,
    state: &ToolState,
    workspace: &Path,
    list: &todo::TodoList,
) -> Result<(), String> {
    list.save(workspace)?;
    let _ = app.emit(
        "agent://todo-update",
        super::TodoUpdateEvent {
            items: list.items.clone(),
            updated_at: list.updated_at,
        },
    );
    let _ = state;
    Ok(())
}

async fn set_todo_list(
    app: &AppHandle,
    state: &ToolState,
    items: &[String],
) -> Result<ToolResult, String> {
    let workspace = resolve_root(state, None).await?;
    if items.is_empty() {
        return Err("set_todo_list needs at least one item.".into());
    }
    let list = todo::new_list(items.to_vec());
    let open = list.open_count();
    save_and_emit_todos(app, state, &workspace, &list)?;
    Ok(ToolResult::ok(
        "set_todo_list",
        format!(
            "Todo list set: {} item(s), {open} open. Saved to `.ai/todos.json`.",
            list.items.len()
        ),
        Some(list.render()),
        Some(json!({ "total": list.items.len(), "open": open })),
    ))
}

async fn get_todo_list(state: &ToolState) -> Result<ToolResult, String> {
    let list = load_todos(state).await?;
    let open = list.open_count();
    Ok(ToolResult::ok(
        "get_todo_list",
        format!("Todo list: {} open / {} total.", open, list.items.len()),
        Some(list.render()),
        Some(json!({ "total": list.items.len(), "open": open })),
    ))
}

async fn mark_todo_item_done(
    app: &AppHandle,
    state: &ToolState,
    item: usize,
) -> Result<ToolResult, String> {
    let workspace = resolve_root(state, None).await?;
    let mut list = load_todos(state).await?;
    if list.items.is_empty() {
        return Err("The todo list is empty. Call `set_todo_list` first.".into());
    }
    let total = list.items.len();
    if item == 0 || item > total {
        return Err(format!(
            "Todo #{item} not found (the list has {total} items)."
        ));
    }
    let target = &mut list.items[item - 1];
    let title = target.title.clone();
    if target.done {
        return Ok(ToolResult::ok(
            "mark_todo_item_done",
            format!("Todo #{item} `{title}` was already done."),
            Some(list.render()),
            Some(json!({ "total": list.items.len(), "open": list.open_count() })),
        ));
    }
    target.done = true;
    list.updated_at = now_ms();
    let open = list.open_count();
    save_and_emit_todos(app, state, &workspace, &list)?;
    Ok(ToolResult::ok(
        "mark_todo_item_done",
        format!("Marked todo #{item} `{title}` done. {open} item(s) still open."),
        Some(list.render()),
        Some(json!({ "total": list.items.len(), "open": open })),
    ))
}

// ---------------------------------------------------------------------------
// Bionic §3.2 WEB tools: web_search / web_extract / download_file (BN-3).
//
// Security contract (Bionic §3.3):
//   * public http(s) only — credentials in URLs are rejected,
//   * every target host must resolve to a PUBLIC address (no loopback /
//     private / link-local — SSRF guard),
//   * redirects are followed manually and re-validated at every hop,
//   * `download_file` is approval-every-call (see policy::always_ask), writes
//     only NEW files inside the workspace, and caps at MAX_DOWNLOAD_BYTES.
// ---------------------------------------------------------------------------

const WEB_TIMEOUT: Duration = Duration::from_secs(30);
const WEB_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// Cap on HTML/text bodies pulled by search + extract.
const MAX_EXTRACT_BYTES: usize = 2 * 1024 * 1024;
/// Hard cap for `download_file` (Bionic §3.2: ≤100 MiB).
const MAX_DOWNLOAD_BYTES: u64 = 100 * 1024 * 1024;
/// Cap on the text handed back to the model from extract/search pages.
const MAX_EXTRACT_CHARS: usize = 20_000;
const MAX_REDIRECTS: usize = 5;

/// Minimal percent-encoding for query parameters.
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Percent-decode a URL component (lossy UTF-8).
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

fn http_client(with_total_timeout: bool) -> Result<reqwest::Client, String> {
    // Redirects are disabled here and followed manually with per-hop
    // validation (`get_following_redirects`) so a redirect can never bounce a
    // request at a private address.
    let mut b = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(WEB_CONNECT_TIMEOUT)
        .user_agent("Mozilla/5.0 (compatible; ai-editor-local-agent/1.0)");
    if with_total_timeout {
        b = b.timeout(WEB_TIMEOUT);
    }
    b.build()
        .map_err(|e| format!("Failed to build HTTP client: {e}"))
}

/// Pure host check used by [`validate_public_http_url`]: every resolved
/// address must be globally reachable (not loopback / private / link-local /
/// unspecified, not IPv6 unique-local or IPv4-mapped private).
fn assert_host_public(host: &str, addrs: &[std::net::IpAddr]) -> Result<(), String> {
    use std::net::IpAddr;
    if addrs.is_empty() {
        return Err(format!("Host `{host}` did not resolve to any address."));
    }
    for ip in addrs {
        let banned = match ip {
            IpAddr::V4(v4) => {
                v4.is_loopback()
                    || v4.is_private()
                    || v4.is_link_local()
                    || v4.is_unspecified()
                    || v4.is_broadcast()
                    || v4.octets()[0] == 0 // "this network" 0.0.0.0/8
            }
            IpAddr::V6(v6) => {
                v6.is_loopback()
                    || v6.is_unspecified()
                    // IPv4-mapped (::ffff:10.0.0.1 etc.) — check embedded v4.
                    || match v6.to_ipv4_mapped() {
                        Some(v4) => v4.is_loopback() || v4.is_private() || v4.is_link_local(),
                        None => (v6.segments()[0] & 0xfe00) == 0xfc00 // unique-local fc00::/7
                            || (v6.segments()[0] & 0xffc0) == 0xfe80, // link-local fe80::/10
                    }
            }
        };
        if banned {
            return Err(format!(
                "Host `{host}` resolves to a non-public address ({ip}); refusing to connect."
            ));
        }
    }
    Ok(())
}

/// Validate that `raw` is a public http(s) URL without embedded credentials,
/// resolving DNS when needed (SSRF guard).
fn validate_public_http_url(raw: &str) -> Result<reqwest::Url, String> {
    let url = reqwest::Url::parse(raw.trim()).map_err(|e| format!("Invalid URL `{raw}`: {e}"))?;
    if url.scheme() != "http" && url.scheme() != "https" {
        return Err(format!(
            "Only http(s) URLs are allowed (got scheme `{}`).",
            url.scheme()
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("URLs with embedded credentials (`user:pass@host`) are rejected.".into());
    }
    let Some(host) = url.host_str() else {
        return Err("URL has no host.".into());
    };
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        assert_host_public(host, &[ip])?;
    } else {
        let port = url.port_or_known_default().unwrap_or(80);
        use std::net::ToSocketAddrs;
        let addrs: Vec<std::net::IpAddr> = (host, port)
            .to_socket_addrs()
            .map_err(|e| format!("Failed to resolve host `{host}`: {e}"))?
            .map(|s| s.ip())
            .collect();
        assert_host_public(host, &addrs)?;
    }
    Ok(url)
}

/// GET with manual redirect handling; every hop is re-validated against the
/// SSRF guard before being requested.
async fn get_following_redirects(
    client: &reqwest::Client,
    url: &reqwest::Url,
) -> Result<reqwest::Response, String> {
    let mut current = url.clone();
    for _ in 0..=MAX_REDIRECTS {
        let resp = client
            .get(current.clone())
            .send()
            .await
            .map_err(|e| format!("Request to `{current}` failed: {e}"))?;
        if resp.status().is_redirection() {
            let loc = resp
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| format!("Redirect from `{current}` has no Location header."))?
                .to_string();
            let next = current
                .join(&loc)
                .map_err(|e| format!("Invalid redirect target `{loc}`: {e}"))?;
            current = validate_public_http_url(next.as_str())?;
            continue;
        }
        return Ok(resp);
    }
    Err(format!(
        "Too many redirects (>{MAX_REDIRECTS}) fetching `{url}`."
    ))
}

/// Decode the five entities html_to_text actually meets in the wild plus the
/// numeric forms; everything else passes through unchanged.
fn decode_entities(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(pos) = rest.find('&') {
        out.push_str(&rest[..pos]);
        let after = &rest[pos..];
        let end = after.find(';').map(|p| p + 1).unwrap_or(after.len());
        let entity = &after[..end];
        let decoded = match entity {
            "&amp;" => Some('&'),
            "&lt;" => Some('<'),
            "&gt;" => Some('>'),
            "&quot;" => Some('"'),
            "&#39;" | "&apos;" | "&#x27;" | "&#X27;" => Some('\''),
            "&nbsp;" => Some('\u{00a0}'),
            _ => None,
        };
        match decoded {
            Some(c) => out.push(c),
            None => {
                // Numeric forms: &#123; / &#x1F600;
                let body = entity.strip_prefix("&#").and_then(|b| b.strip_suffix(';'));
                let c = body.and_then(|b| {
                    if let Some(hex) = b.strip_prefix('x').or_else(|| b.strip_prefix('X')) {
                        u32::from_str_radix(hex, 16).ok()
                    } else {
                        b.parse::<u32>().ok()
                    }
                });
                match c.and_then(char::from_u32) {
                    Some(ch) => out.push(ch),
                    None => out.push_str(entity),
                }
            }
        }
        rest = &rest[pos + end.max(1)..];
        if end == 0 {
            break;
        }
    }
    out.push_str(rest);
    out
}

/// Strip tags from an HTML fragment (titles/snippets): comments, script/style
/// blocks removed, tags dropped, whitespace collapsed.
fn strip_tags_fragment(html: &str) -> String {
    let mut no_comments = String::with_capacity(html.len());
    let mut rest = html;
    loop {
        match rest.find("<!--") {
            Some(start) => {
                no_comments.push_str(&rest[..start]);
                match rest[start..].find("-->") {
                    Some(rel) => rest = &rest[start + rel + 3..],
                    None => break,
                }
            }
            None => {
                no_comments.push_str(rest);
                break;
            }
        }
    }
    let mut out = String::with_capacity(no_comments.len());
    let mut inside_tag = false;
    for c in no_comments.chars() {
        match c {
            '<' => inside_tag = true,
            '>' => inside_tag = false,
            _ if !inside_tag => out.push(c),
            _ => {}
        }
    }
    decode_entities(&out.split_whitespace().collect::<Vec<_>>().join(" "))
}

/// Convert an HTML document to readable plain text: head/script/style/noscript
/// stripped, block boundaries become newlines, remaining tags dropped.
fn html_to_text(html: &str) -> String {
    let mut work = html.to_string();
    // Remove whole elements whose content must never surface (iterate with
    // fresh scans until stable so repeated/nested blocks are all gone).
    loop {
        let mut removed = false;
        let low = work.to_lowercase();
        for tag in ["script", "style", "noscript", "head", "svg", "iframe"] {
            let open = format!("<{tag}");
            let close = format!("</{tag}>");
            if let Some(start) = low.find(&open) {
                if let Some(rel_end) = low[start..].find(&close) {
                    let end = start + rel_end + close.len();
                    work.replace_range(start..end, " ");
                    removed = true;
                }
            }
        }
        if !removed {
            break;
        }
    }
    let mut text = String::with_capacity(work.len());
    let mut chars = work.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if c == '<' {
            let tag_end = work[i..].find('>').map(|p| i + p);
            let tag = tag_end
                .map(|te| work[i..te].to_lowercase())
                .unwrap_or_default();
            let newline_tags = [
                "<p",
                "<br",
                "<li",
                "<tr",
                "<div",
                "<h1",
                "<h2",
                "<h3",
                "<h4",
                "<h5",
                "<h6",
                "<section",
                "<article",
                "<table",
                "<ul",
                "<ol",
                "<pre",
                "<blockquote",
            ];
            if newline_tags.iter().any(|t| tag.starts_with(t)) {
                text.push('\n');
            }
            // Skip the tag body up to '>'.
            if let Some(te) = tag_end {
                for (j, _) in chars.by_ref() {
                    if j >= te {
                        break;
                    }
                }
            }
            continue;
        }
        text.push(c);
    }
    let decoded = decode_entities(&text);
    // Collapse runs of blank lines; trim each line's indentation.
    let mut out: Vec<String> = Vec::new();
    for line in decoded.lines() {
        let t = line.trim();
        if t.is_empty() && out.last().map(|l: &String| l.is_empty()).unwrap_or(true) {
            continue;
        }
        out.push(t.to_string());
    }
    out.join("\n").trim().to_string()
}

/// Extract the real target from a DuckDuckGo redirect link (`…?uddg=<enc>`),
/// passing plain links through untouched.
fn ddg_real_href(href: &str) -> String {
    if let Some(idx) = href.find("uddg=") {
        let rest = &href[idx + 5..];
        let enc = rest.split('&').next().unwrap_or(rest);
        let decoded = percent_decode(enc);
        if decoded.starts_with("http") {
            return decoded;
        }
    }
    percent_decode(href)
}

#[derive(Debug)]
struct WebSearchResult {
    title: String,
    href: String,
    snippet: String,
}

/// Parse DuckDuckGo HTML results (`html.duckduckgo.com/html/?q=`).
fn parse_ddg_html(page: &str) -> Vec<WebSearchResult> {
    let mut out = Vec::new();
    let re_link = regex::Regex::new(r#"<a([^>]*)class="result__a"([^>]*)>([\s\S]*?)</a>"#).unwrap();
    let re_snip =
        regex::Regex::new(r#"<a[^>]*class="result__snippet"[^>]*>([\s\S]*?)</a>"#).unwrap();
    let re_href = regex::Regex::new(r#"href\s*=\s*"([^"]+)""#).unwrap();
    let snippets: Vec<String> = re_snip
        .captures_iter(page)
        .map(|c| strip_tags_fragment(&c[1]))
        .collect();
    for (i, cap) in re_link.captures_iter(page).enumerate() {
        let attrs = format!("{}{}", &cap[1], &cap[2]);
        let Some(href_cap) = re_href.captures(&attrs) else {
            continue;
        };
        let href = ddg_real_href(href_cap[1].trim());
        if !href.starts_with("http") {
            continue;
        }
        out.push(WebSearchResult {
            title: strip_tags_fragment(&cap[3]),
            href,
            snippet: snippets.get(i).cloned().unwrap_or_default(),
        });
    }
    out
}

/// Fallback parser for `lite.duckduckgo.com` results.
fn parse_ddg_lite(page: &str) -> Vec<WebSearchResult> {
    let mut out = Vec::new();
    let re_link = regex::Regex::new(
        r#"<a[^>]*class=['"]result-link['"][^>]*href="([^"]+)"[^>]*>([\s\S]*?)</a>"#,
    )
    .unwrap();
    let re_snip =
        regex::Regex::new(r#"<td[^>]*class=['"]result-snippet['"][^>]*>([\s\S]*?)</td>"#).unwrap();
    let snippets: Vec<String> = re_snip
        .captures_iter(page)
        .map(|c| strip_tags_fragment(&c[1]))
        .collect();
    for (i, cap) in re_link.captures_iter(page).enumerate() {
        let href = ddg_real_href(cap[1].trim());
        if !href.starts_with("http") {
            continue;
        }
        out.push(WebSearchResult {
            title: strip_tags_fragment(&cap[2]),
            href,
            snippet: snippets.get(i).cloned().unwrap_or_default(),
        });
    }
    out
}

fn render_search_results(results: &[WebSearchResult]) -> String {
    let mut out = String::new();
    for (i, r) in results.iter().enumerate() {
        out.push_str(&format!("{}. {}\n   {}\n", i + 1, r.title, r.href));
        if !r.snippet.is_empty() {
            out.push_str(&format!("   {}\n", r.snippet));
        }
        out.push('\n');
    }
    out
}

async fn web_search(
    _state: &ToolState,
    interrupt: &CancellationToken,
    query: &str,
    max_results: usize,
) -> Result<ToolResult, String> {
    let query = query.trim();
    if query.is_empty() {
        return Err("web_search needs a non-empty query.".into());
    }
    let max_results = max_results.clamp(1, 10);
    let client = http_client(true)?;
    // Primary endpoint + lite fallback; both validated through the SSRF guard.
    let endpoints = [
        format!("https://html.duckduckgo.com/html/?q={}", urlencode(query)),
        format!("https://lite.duckduckgo.com/lite/?q={}", urlencode(query)),
    ];
    let mut last_err = String::new();
    for endpoint in endpoints {
        if interrupt.is_cancelled() {
            return Err("Execution Aborted".into());
        }
        let url = validate_public_http_url(&endpoint)?;
        match get_following_redirects(&client, &url).await {
            Ok(resp) if resp.status().is_success() => {
                let body = resp.text().await.unwrap_or_default();
                let mut results = parse_ddg_html(&body);
                if results.is_empty() {
                    results = parse_ddg_lite(&body);
                }
                results.truncate(max_results);
                if results.is_empty() {
                    last_err = format!("No results parsed from {endpoint}.");
                    continue;
                }
                let rendered = render_search_results(&results);
                return Ok(ToolResult::ok(
                    "web_search",
                    format!("Web search `{query}`: {} result(s)", results.len()),
                    Some(rendered),
                    Some(json!({ "query": query, "results": results.len(), "engine": endpoint })),
                ));
            }
            Ok(resp) => {
                last_err = format!("{endpoint} returned HTTP {}.", resp.status());
            }
            Err(e) => last_err = e,
        }
    }
    Err(format!("web_search failed: {last_err}"))
}

async fn web_extract(
    _state: &ToolState,
    interrupt: &CancellationToken,
    raw_url: &str,
) -> Result<ToolResult, String> {
    let url = validate_public_http_url(raw_url)?;
    let client = http_client(true)?;
    let resp = get_following_redirects(&client, &url).await?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {} fetching `{url}`.", resp.status()));
    }
    let ctype = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_lowercase();
    let ok_type = ctype.starts_with("text/")
        || ctype.contains("html")
        || ctype.contains("xml")
        || ctype.contains("json");
    if !ok_type {
        return Err(format!(
            "`{url}` serves `{ctype}`, which is not text. Use download_file instead."
        ));
    }
    // Read at most MAX_EXTRACT_BYTES (+1 to detect truncation).
    let mut bytes: Vec<u8> = Vec::new();
    let mut resp = resp;
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|e| format!("Read failed: {e}"))?
    {
        if interrupt.is_cancelled() {
            return Err("Execution Aborted".into());
        }
        if bytes.len() + chunk.len() > MAX_EXTRACT_BYTES {
            bytes.extend_from_slice(
                &chunk[..chunk
                    .len()
                    .min(MAX_EXTRACT_BYTES.saturating_sub(bytes.len()))],
            );
            break;
        }
        bytes.extend_from_slice(&chunk);
        if bytes.len() >= MAX_EXTRACT_BYTES {
            break;
        }
    }
    let truncated_body = bytes.len() >= MAX_EXTRACT_BYTES;
    let text = String::from_utf8_lossy(&bytes).to_string();
    let head = text.trim_start().to_lowercase();
    let looks_html =
        head.starts_with("<!doctype html") || head.starts_with("<html") || ctype.contains("html");
    let plain = if looks_html {
        html_to_text(&text)
    } else {
        text
    };
    let total_chars = plain.chars().count();
    let mut out: String = plain.chars().take(MAX_EXTRACT_CHARS).collect();
    if total_chars > MAX_EXTRACT_CHARS || truncated_body {
        out.push_str(&format!(
            "\n\n[Truncated at {MAX_EXTRACT_CHARS} chars{}]",
            if truncated_body {
                " (page size capped)"
            } else {
                ""
            }
        ));
    }
    if out.trim().is_empty() {
        return Err(format!("`{url}` yielded no readable text."));
    }
    Ok(ToolResult::ok(
        "web_extract",
        format!("Extracted {total_chars} chars from `{url}`"),
        Some(out),
        Some(json!({ "url": url.as_str(), "contentType": ctype, "chars": total_chars })),
    ))
}

async fn download_file_tool(
    state: &ToolState,
    interrupt: &CancellationToken,
    raw_url: &str,
    path: &str,
) -> Result<ToolResult, String> {
    let url = validate_public_http_url(raw_url)?;
    let root = resolve_root(state, None).await?;
    let target = abs_from(&root, path)?;
    if !target.exists() {
        // Brand-new file — fine. But its parent chain must stay in-workspace.
        let parent = target
            .parent()
            .filter(|p| p.to_string_lossy().starts_with(&*root.to_string_lossy()))
            .ok_or_else(|| format!("Destination `{path}` is outside the workspace."))?;
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| format!("Cannot create {}: {e}", parent.display()))?;
    } else {
        return Err(format!(
            "Refusing to overwrite existing file `{}`. Pick a path that does not exist yet.",
            target.display()
        ));
    }
    let client = http_client(false)?;
    let mut resp = get_following_redirects(&client, &url).await?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {} fetching `{url}`.", resp.status()));
    }
    if let Some(len) = resp.content_length() {
        if len > MAX_DOWNLOAD_BYTES {
            return Err(format!(
                "Download is {len} bytes; the cap is {MAX_DOWNLOAD_BYTES} bytes (100 MiB)."
            ));
        }
    }
    let mut file = tokio::fs::File::create(&target)
        .await
        .map_err(|e| format!("Cannot create {}: {e}", target.display()))?;
    use tokio::io::AsyncWriteExt;
    let mut written: u64 = 0;
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|e| format!("Download failed: {e}"))?
    {
        if interrupt.is_cancelled() {
            drop(file);
            let _ = tokio::fs::remove_file(&target).await;
            return Err("Execution Aborted".into());
        }
        written += chunk.len() as u64;
        if written > MAX_DOWNLOAD_BYTES {
            drop(file);
            let _ = tokio::fs::remove_file(&target).await;
            return Err(format!(
                "Download exceeded the {MAX_DOWNLOAD_BYTES}-byte cap; aborted and removed the partial file."
            ));
        }
        file.write_all(&chunk)
            .await
            .map_err(|e| format!("Write failed: {e}"))?;
    }
    file.flush().await.ok();
    drop(file);
    Ok(ToolResult::ok(
        "download_file",
        format!("Downloaded {written} bytes → `{}`", target.display()),
        Some(format!(
            "Saved {written} bytes from `{url}` to `{}`.",
            target.display()
        )),
        Some(json!({
            "url": url.as_str(),
            "path": target.to_string_lossy(),
            "bytes": written,
        })),
    ))
}

// ---------------------------------------------------------------------------
// Bionic §3.2 sandboxed code execution: run_python / run_javascript (BN-4).
//
// Decision (documented in PROJECT_STATUS.md in lieu of bundling Pyodide/Deno):
//   * interpreters are DISCOVERED on PATH at call time (never bundled),
//   * Python runs isolated (`-I`: ignores PYTHONPATH / user site / env vars)
//     with the script + cwd inside a per-session scratchpad dir,
//   * Deno runs with an exact lockdown flag set (fs limited to cwd, no net /
//     env / sys info / subprocesses / FFI, no prompts); Node.js >= 20 is a
//     fallback behind its `--permission` model,
//   * a clear typed error is returned when no interpreter is installed,
//   * every run has a hard timeout and races the circuit-breaker token.
// ---------------------------------------------------------------------------

const CODE_RUN_DEFAULT_TIMEOUT: u64 = 30;
const CODE_RUN_MAX_TIMEOUT: u64 = 120;
const MAX_CODE_OUTPUT: usize = 8_000;

#[derive(Debug)]
struct Interpreter {
    program: String,
    args: Vec<String>,
    runtime: &'static str,
}

/// Look up an executable on PATH and confirm it runs (`--version`).
async fn discover(program: &str, version_args: &[&str]) -> Option<String> {
    let mut cmd = tokio::process::Command::new(program);
    cmd.args(version_args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let out = tokio::time::timeout(Duration::from_secs(10), cmd.output())
        .await
        .ok()?
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (!text.trim().is_empty()).then_some(text.lines().next().unwrap_or("").trim().to_string())
}

async fn discover_python() -> Option<Interpreter> {
    for candidate in ["python", "python3"] {
        if let Some(v) = discover(candidate, &["--version"]).await {
            if v.to_lowercase().contains("python") {
                return Some(Interpreter {
                    program: candidate.into(),
                    args: vec!["-I".into()],
                    runtime: "python",
                });
            }
        }
    }
    // Windows py launcher.
    if let Some(v) = discover("py", &["-3", "--version"]).await {
        if v.to_lowercase().contains("python") {
            return Some(Interpreter {
                program: "py".into(),
                args: vec!["-3".into(), "-I".into()],
                runtime: "python",
            });
        }
    }
    None
}

/// The exact Bionic §3.2 lockdown flag set for Deno.
fn deno_lockdown_args(script_name: &str) -> Vec<String> {
    [
        "--allow-read=.",
        "--allow-write=.",
        "--no-prompt",
        "--deny-net",
        "--deny-env",
        "--deny-sys",
        "--deny-run",
        "--deny-ffi",
    ]
    .iter()
    .map(|s| s.to_string())
    .chain(std::iter::once(script_name.to_string()))
    .collect()
}

async fn discover_deno() -> Option<Interpreter> {
    discover("deno", &["--version"]).await.map(|_| Interpreter {
        program: "deno".into(),
        args: vec!["run".into()], // lockdown flags appended per-run (script name last)
        runtime: "deno",
    })
}

async fn discover_node() -> Option<Interpreter> {
    let v = discover("node", &["--version"]).await?;
    let major = v
        .trim_start_matches('v')
        .split('.')
        .next()
        .and_then(|m| m.parse::<u32>().ok())
        .unwrap_or(0);
    (major >= 20).then(|| Interpreter {
        program: "node".into(),
        args: vec!["--permission".into()],
        runtime: "node",
    })
}

/// Shared runner: write `code` into the per-call scratchpad dir, spawn with
/// `interp`, race timeout vs. interrupt, cap output. Returns
/// `(exit_ok, stdout, stderr, runtime_label)`.
async fn run_sandboxed(
    state: &ToolState,
    interp: &Interpreter,
    code: &str,
    file_name: &str,
    timeout_secs: Option<u64>,
    interrupt: &CancellationToken,
) -> Result<(bool, String, String, String), String> {
    let session_id = state.session_id.load(std::sync::atomic::Ordering::SeqCst);
    let dir = super::session_scratchpad(session_id).join(format!("run-{}", now_ms()));
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| format!("Cannot create run dir {}: {e}", dir.display()))?;
    let script = dir.join(file_name);
    tokio::fs::write(&script, code)
        .await
        .map_err(|e| format!("Cannot write {}: {e}", script.display()))?;

    let mut cmd = tokio::process::Command::new(&interp.program);
    match interp.runtime {
        "deno" => {
            cmd.args(interp.args.iter())
                .args(deno_lockdown_args(file_name))
                .current_dir(&dir);
        }
        _ => {
            cmd.args(interp.args.iter())
                .arg(file_name)
                .current_dir(&dir);
        }
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    // Minimal environment: never leak the user's secrets/env to sandboxed code.
    // On Windows SystemRoot is required for process init, so keep an allowlist.
    cmd.env_clear();
    for key in [
        "PATH",
        "TEMP",
        "TMP",
        #[cfg(windows)]
        "SystemRoot",
        #[cfg(windows)]
        "SYSTEMDRIVE",
        #[cfg(not(windows))]
        "HOME",
    ] {
        if let Ok(v) = std::env::var(key) {
            cmd.env(key, v);
        }
    }
    #[cfg(windows)]
    {
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("Failed to start `{}`: {e}", interp.program))?;
    let pid = child.id();
    let stdout = child.stdout.take().expect("stdout piped");
    let stderr = child.stderr.take().expect("stderr piped");

    use tokio::io::AsyncReadExt;
    let task = async move {
        let mut so = String::new();
        let mut se = String::new();
        let (mut so_pipe, mut se_pipe) = (stdout, stderr);
        let mut so_buf = vec![0u8; 4096];
        let mut se_buf = vec![0u8; 4096];
        let mut so_open = true;
        let mut se_open = true;
        // Interleave both pipes so a chatty stderr can't deadlock the child.
        while so_open || se_open {
            tokio::select! {
                n = so_pipe.read(&mut so_buf), if so_open => match n {
                    Ok(0) | Err(_) => so_open = false,
                    Ok(n) => {
                        so.push_str(&String::from_utf8_lossy(&so_buf[..n]));
                        if so.len() > MAX_CODE_OUTPUT { so_open = false; }
                    }
                },
                n = se_pipe.read(&mut se_buf), if se_open => match n {
                    Ok(0) | Err(_) => se_open = false,
                    Ok(n) => {
                        se.push_str(&String::from_utf8_lossy(&se_buf[..n]));
                        if se.len() > MAX_CODE_OUTPUT { se_open = false; }
                    }
                },
                else => break,
            }
        }
        let status = child.wait().await;
        (status, so, se)
    };

    let timeout = Duration::from_secs(
        timeout_secs
            .unwrap_or(CODE_RUN_DEFAULT_TIMEOUT)
            .clamp(1, CODE_RUN_MAX_TIMEOUT),
    );

    enum RunOutcome {
        Finished(std::io::Result<std::process::ExitStatus>, String, String),
        TimedOut,
    }
    let outcome = tokio::select! {
        r = task => {
            let (status, so, se) = r;
            RunOutcome::Finished(status, so, se)
        }
        _ = tokio::time::sleep(timeout) => RunOutcome::TimedOut,
        _ = interrupt.clone().cancelled_owned() => {
            let _ = kill_tree(pid);
            return Err(super::interrupt::ABORT_REASON.to_string());
        }
    };
    // Clean the run dir regardless of outcome.
    let _ = tokio::fs::remove_dir_all(&dir).await;

    match outcome {
        RunOutcome::TimedOut => {
            let _ = kill_tree(pid);
            Err(format!(
                "Execution timed out after {}s. Retry with a larger `timeout_secs` or optimize the code.",
                timeout.as_secs()
            ))
        }
        RunOutcome::Finished(status, so, se) => Ok((
            status.map(|s| s.success()).unwrap_or(false),
            so,
            se,
            interp.runtime.to_string(),
        )),
    }
}

fn render_code_result(
    tool: &str,
    label: &str,
    ok: bool,
    stdout: String,
    stderr: String,
    runtime: &str,
) -> ToolResult {
    let mut out = String::new();
    if !stdout.trim().is_empty() {
        out.push_str(stdout.trim_end());
        out.push('\n');
    }
    if !stderr.trim().is_empty() {
        out.push_str("--- stderr ---\n");
        out.push_str(stderr.trim_end());
        out.push('\n');
    }
    if out.is_empty() {
        out = "(no output)".to_string();
    }
    let summary = if ok {
        format!("{label} finished via {runtime}")
    } else {
        format!("{label} failed (exit != 0, runtime {runtime})")
    };
    ToolResult {
        success: ok,
        tool: tool.to_string(),
        summary,
        stdout: Some(out),
        error: if ok {
            None
        } else {
            Some(stderr.trim_end().to_string())
        },
        stats: Some(json!({ "runtime": runtime })),
        duration_ms: 0,
    }
}

async fn run_python(
    state: &ToolState,
    interrupt: &CancellationToken,
    code: &str,
    timeout_secs: Option<u64>,
) -> Result<ToolResult, String> {
    let _ = state;
    let Some(interp) = discover_python().await else {
        return Err(concat!(
            "NO_INTERPRETER: no Python found on PATH ",
            "(tried `python`, `python3`, `py -3`). Install Python 3 to enable run_python."
        )
        .to_string());
    };
    let (ok, so, se, rt) =
        run_sandboxed(state, &interp, code, "script.py", timeout_secs, interrupt).await?;
    Ok(render_code_result(
        "run_python",
        "Python script",
        ok,
        so,
        se,
        &rt,
    ))
}

async fn run_javascript(
    state: &ToolState,
    interrupt: &CancellationToken,
    code: &str,
    timeout_secs: Option<u64>,
) -> Result<ToolResult, String> {
    let _ = state;
    let (interp, file_name) = if let Some(deno) = discover_deno().await {
        (deno, "script.ts")
    } else if let Some(node) = discover_node().await {
        (node, "script.js")
    } else {
        return Err(concat!(
            "NO_INTERPRETER: neither Deno nor Node.js >= 20 found on PATH. ",
            "Install Deno (preferred — strict sandbox flags) or Node.js 20+ to enable run_javascript."
        )
        .to_string());
    };
    let (ok, so, se, rt) =
        run_sandboxed(state, &interp, code, file_name, timeout_secs, interrupt).await?;
    Ok(render_code_result(
        "run_javascript",
        "JavaScript module",
        ok,
        so,
        se,
        &rt,
    ))
}

/// Deterministic arithmetic evaluator for the `calculate` tool: a tiny
/// recursive-descent parser over `+ - * / % ^`, parentheses, unary minus and
/// f64 literals. No subprocess, no approval gate — safe for ROUTINE policy.
pub(crate) fn eval_arithmetic(expr: &str) -> Result<f64, String> {
    #[derive(Clone, Copy)]
    enum Tok<'a> {
        Num(f64),
        Op(char),
        LParen,
        RParen,
        Ident(&'a str),
    }

    fn tokenize(src: &str) -> Result<Vec<Tok<'_>>, String> {
        let b: Vec<char> = src.chars().collect();
        let mut toks = Vec::new();
        let mut i = 0;
        while i < b.len() {
            match b[i] {
                c if c.is_whitespace() => i += 1,
                '+' | '-' | '*' | '/' | '%' | '^' => {
                    toks.push(Tok::Op(b[i]));
                    i += 1;
                }
                '(' => {
                    toks.push(Tok::LParen);
                    i += 1;
                }
                ')' => {
                    toks.push(Tok::RParen);
                    i += 1;
                }
                c if c.is_ascii_digit() || c == '.' => {
                    let start = i;
                    while i < b.len() && (b[i].is_ascii_digit() || b[i] == '.') {
                        i += 1;
                    }
                    let text: String = b[start..i].iter().collect();
                    let n = text
                        .parse::<f64>()
                        .map_err(|_| format!("invalid number `{text}`"))?;
                    toks.push(Tok::Num(n));
                }
                c if c.is_alphabetic() => {
                    // Constants only; anything else is rejected so the model
                    // cannot smuggle code into an arithmetic-only tool.
                    let start = i;
                    while i < b.len() && b[i].is_alphanumeric() {
                        i += 1;
                    }
                    toks.push(Tok::Ident(&src[start..i]));
                }
                other => return Err(format!("unexpected character `{other}`")),
            }
        }
        Ok(toks)
    }

    fn ident_value(name: &str) -> Option<f64> {
        match name.to_ascii_lowercase().as_str() {
            "pi" => Some(std::f64::consts::PI),
            "e" => Some(std::f64::consts::E),
            _ => None,
        }
    }

    struct Parser<'a> {
        toks: &'a [Tok<'a>],
        pos: usize,
    }

    impl Parser<'_> {
        fn peek(&self) -> Option<&Tok<'_>> {
            self.toks.get(self.pos)
        }
        fn next(&mut self) -> Option<&Tok<'_>> {
            let t = self.toks.get(self.pos);
            if t.is_some() {
                self.pos += 1;
            }
            t
        }

        // expr := term (('+'|'-') term)*
        fn expr(&mut self) -> Result<f64, String> {
            let mut v = self.term()?;
            loop {
                match self.peek() {
                    Some(Tok::Op('+')) => {
                        self.next();
                        v += self.term()?;
                    }
                    Some(Tok::Op('-')) => {
                        self.next();
                        v -= self.term()?;
                    }
                    _ => return Ok(v),
                }
            }
        }

        // term := unary (('*'|'/'|'%') unary)*
        fn term(&mut self) -> Result<f64, String> {
            let mut v = self.unary()?;
            loop {
                match self.peek() {
                    Some(Tok::Op('*')) => {
                        self.next();
                        v *= self.unary()?;
                    }
                    Some(Tok::Op('/')) => {
                        self.next();
                        let d = self.unary()?;
                        if d == 0.0 {
                            return Err("division by zero".into());
                        }
                        v /= d;
                    }
                    Some(Tok::Op('%')) => {
                        self.next();
                        let d = self.unary()?;
                        if d == 0.0 {
                            return Err("modulo by zero".into());
                        }
                        v %= d;
                    }
                    _ => return Ok(v),
                }
            }
        }

        // unary := ('-'|'+')* power
        fn unary(&mut self) -> Result<f64, String> {
            if matches!(self.peek(), Some(Tok::Op('-'))) {
                self.next();
                return Ok(-self.unary()?);
            }
            if matches!(self.peek(), Some(Tok::Op('+'))) {
                self.next();
                return self.unary();
            }
            self.power()
        }

        // power := atom ('^' unary)?   (right-associative)
        fn power(&mut self) -> Result<f64, String> {
            let base = self.atom()?;
            if matches!(self.peek(), Some(Tok::Op('^'))) {
                self.next();
                return Ok(base.powf(self.unary()?));
            }
            Ok(base)
        }

        fn atom(&mut self) -> Result<f64, String> {
            match self.next() {
                Some(Tok::Num(n)) => Ok(*n),
                Some(Tok::Ident(name)) => {
                    ident_value(name).ok_or_else(|| format!("unknown constant `{name}`"))
                }
                Some(Tok::LParen) => {
                    let v = self.expr()?;
                    match self.next() {
                        Some(Tok::RParen) => Ok(v),
                        _ => Err("missing closing parenthesis".into()),
                    }
                }
                _ => Err("expected a number or `(`".into()),
            }
        }
    }

    let toks = tokenize(expr)?;
    if toks.is_empty() {
        return Err("empty expression".into());
    }
    let mut p = Parser {
        toks: &toks,
        pos: 0,
    };
    let value = p.expr()?;
    if p.pos != toks.len() {
        return Err(format!(
            "trailing input at token {} of {}",
            p.pos + 1,
            toks.len()
        ));
    }
    if !value.is_finite() {
        return Err("result is not finite".into());
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arithmetic_evaluates_with_precedence() {
        assert_eq!(eval_arithmetic("2+3"), Ok(5.0));
        assert_eq!(eval_arithmetic("2 + 3 * 4"), Ok(14.0));
        assert_eq!(eval_arithmetic("(2+3)*4"), Ok(20.0));
        assert_eq!(eval_arithmetic("2^3^2"), Ok(512.0)); // right-assoc
        assert_eq!(eval_arithmetic("-5+10"), Ok(5.0));
        assert_eq!(eval_arithmetic("10 % 3"), Ok(1.0));
        assert_eq!(eval_arithmetic("pi"), Ok(std::f64::consts::PI));
    }

    #[test]
    fn arithmetic_rejects_garbage() {
        assert!(eval_arithmetic("").is_err());
        assert!(eval_arithmetic("1/0").is_err());
        assert!(eval_arithmetic("(1+2").is_err());
        assert!(eval_arithmetic("std::env").is_err());
        assert!(eval_arithmetic("__import__").is_err());
    }

    #[test]
    fn sem_tokens_splits_identifiers_and_drops_stopwords() {
        let toks = sem_tokens("authLogin getUserToken the and");
        assert!(toks.contains(&"auth".to_string()));
        assert!(toks.contains(&"login".to_string()));
        assert!(toks.contains(&"get".to_string()));
        assert!(toks.contains(&"user".to_string()));
        assert!(toks.contains(&"token".to_string()));
        assert!(!toks.contains(&"the".to_string()));
        assert!(!toks.contains(&"and".to_string()));
        assert!(!toks.contains(&"authlogin".to_string()));
    }

    #[test]
    fn sem_tokens_handles_snake_case_and_numbers() {
        let toks = sem_tokens("handle_2fa user_id_42");
        assert!(toks.contains(&"handle".to_string()));
        assert!(toks.contains(&"2fa".to_string()));
        assert!(toks.contains(&"user".to_string()));
        assert!(toks.contains(&"id".to_string()));
        assert!(toks.contains(&"42".to_string()));
    }

    #[test]
    fn sem_tokens_filters_short_noise() {
        let toks = sem_tokens("a b x y");
        assert_eq!(toks, Vec::<String>::new());
    }

    #[test]
    fn count_tf_counts_tokens() {
        let tf = count_tf("foo bar foo");
        assert_eq!(tf.get("foo"), Some(&2));
        assert_eq!(tf.get("bar"), Some(&1));
    }

    #[test]
    fn char_slice_windows_and_continuation() {
        let text = "héllo wörld"; // 11 chars, multi-byte safe
        let (slice, more) = char_slice(text, 0, 5);
        assert_eq!(slice, "héllo");
        assert!(more);
        let (rest, more2) = char_slice(text, 5, 100);
        assert_eq!(rest, " wörld");
        assert!(!more2);
        // Offset past EOF yields empty slice without panicking.
        let (empty, more3) = char_slice(text, 999, 10);
        assert_eq!(empty, "");
        assert!(!more3);
        // Limit is clamped to MAX_READ_CHARS.
        let big = "x".repeat(50_000);
        let (s, m) = char_slice(&big, 0, usize::MAX);
        assert_eq!(s.chars().count(), MAX_READ_CHARS);
        assert!(m);
    }

    #[test]
    fn folder_depth_cap_enforced() {
        let root = Path::new("/ws");
        assert!(folder_depth_ok(root, &root.join("a/b/c")));
        let deep = root.join(
            std::iter::repeat("d")
                .take(50)
                .collect::<Vec<_>>()
                .join("/"),
        );
        assert!(folder_depth_ok(root, &deep));
        let deeper = root.join(
            std::iter::repeat("d")
                .take(51)
                .collect::<Vec<_>>()
                .join("/"),
        );
        assert!(!folder_depth_ok(root, &deeper));
    }

    #[test]
    fn abs_from_resolves_relative_against_root() {
        let root = Path::new("/ws");
        assert_eq!(
            abs_from(root, "src/main.rs").unwrap(),
            PathBuf::from("/ws/src/main.rs")
        );
        assert_eq!(
            abs_from(root, "/etc/hosts").unwrap(),
            PathBuf::from("/etc/hosts")
        );
        assert!(abs_from(root, "   ").is_err());
    }

    #[tokio::test]
    async fn list_dir_orders_dirs_first_with_markers() {
        let tmp = std::env::temp_dir().join(format!("ai-editor-listdir-{}", std::process::id()));
        let dir = tmp.join("entries");
        tokio::fs::create_dir_all(dir.join("zfolder"))
            .await
            .unwrap();
        tokio::fs::create_dir_all(dir.join("afolder"))
            .await
            .unwrap();
        tokio::fs::write(dir.join("b.txt"), "bb").await.unwrap();
        tokio::fs::write(dir.join("a.txt"), "a").await.unwrap();

        let state = ToolState::default();
        *state.workspace.lock().await = Some(tmp.clone());
        let result = list_dir(&state, Some("entries")).await.unwrap();
        let out = result.stdout.unwrap();
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines[0], "afolder/");
        assert_eq!(lines[1], "zfolder/");
        assert_eq!(lines[2], "a.txt (1 bytes)");
        assert_eq!(lines[3], "b.txt (2 bytes)");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[tokio::test]
    async fn copy_move_delete_roundtrip_in_workspace() {
        let tmp = std::env::temp_dir().join(format!("ai-editor-fsops-{}", std::process::id()));
        tokio::fs::create_dir_all(tmp.join("srcdir")).await.unwrap();
        tokio::fs::write(tmp.join("srcdir/note.txt"), "hello")
            .await
            .unwrap();

        let state = ToolState::default();
        *state.workspace.lock().await = Some(tmp.clone());

        // Copy refuses existing destination by default.
        tokio::fs::create_dir_all(tmp.join("dst")).await.unwrap();
        tokio::fs::write(tmp.join("dst/note.txt"), "old")
            .await
            .unwrap();
        let err = copy_file_or_folder(&state, "srcdir/note.txt", "dst/note.txt", false)
            .await
            .unwrap_err();
        assert!(err.contains("canOverwrite"), "unexpected error: {err}");

        // Overwrite copy works.
        copy_file_or_folder(&state, "srcdir/note.txt", "dst/note.txt", true)
            .await
            .unwrap();
        let content = tokio::fs::read_to_string(tmp.join("dst/note.txt"))
            .await
            .unwrap();
        assert_eq!(content, "hello");

        // Move renames.
        move_file_or_folder(&state, "dst/note.txt", "dst/renamed.txt", false)
            .await
            .unwrap();
        assert!(!tmp.join("dst/note.txt").exists());
        assert!(tmp.join("dst/renamed.txt").exists());

        // Delete trashes (file disappears from the workspace).
        delete_file_or_folder(&state, "dst/renamed.txt")
            .await
            .unwrap();
        assert!(!tmp.join("dst/renamed.txt").exists());

        // Workspace root + .ai are protected.
        assert!(delete_file_or_folder(&state, ".").await.is_err());
        tokio::fs::create_dir_all(tmp.join(".ai")).await.unwrap();
        assert!(delete_file_or_folder(&state, ".ai").await.is_err());

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[tokio::test]
    async fn create_folder_enforces_depth_cap() {
        let tmp = std::env::temp_dir().join(format!("ai-editor-mkdir-{}", std::process::id()));
        let state = ToolState::default();
        *state.workspace.lock().await = Some(tmp.clone());
        create_folder(&state, "a/b/c").await.unwrap();
        assert!(tmp.join("a/b/c").is_dir());
        let deep = std::iter::repeat("d")
            .take(51)
            .collect::<Vec<_>>()
            .join("/");
        assert!(create_folder(&state, &deep).await.is_err());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    // ---- BN-3 web tools ----

    #[test]
    fn urlencode_and_percent_decode_roundtrip() {
        let raw = "rust lang \"async fn\" & more/100%";
        let enc = urlencode(raw);
        assert!(!enc.contains(' '));
        assert!(!enc.contains('&'));
        assert_eq!(percent_decode(&enc), raw);
    }

    #[test]
    fn ssrf_guard_rejects_private_and_loopback_hosts() {
        use std::net::IpAddr;
        let v4 = |s: &str| s.parse::<IpAddr>().unwrap();
        assert!(assert_host_public("127.0.0.1", &[v4("127.0.0.1")]).is_err());
        assert!(assert_host_public("10.1.2.3", &[v4("10.1.2.3")]).is_err());
        assert!(assert_host_public("192.168.1.9", &[v4("192.168.1.9")]).is_err());
        assert!(assert_host_public("169.254.1.1", &[v4("169.254.1.1")]).is_err());
        assert!(assert_host_public("0.0.0.0", &[v4("0.0.0.0")]).is_err());
        assert!(assert_host_public("::1", &[v4("::1")]).is_err());
        assert!(
            assert_host_public("host", &[v4("fe80::1")]).is_err(),
            "link-local v6 must be rejected"
        );
        assert!(
            assert_host_public("host", &[v4("fd00::1")]).is_err(),
            "unique-local v6 must be rejected"
        );
        assert!(
            assert_host_public("host", &[v4("::ffff:127.0.0.1")]).is_err(),
            "v4-mapped loopback must be rejected"
        );
        assert!(assert_host_public("1.1.1.1", &[v4("1.1.1.1")]).is_ok());
        assert!(assert_host_public("host", &[v4("2606:4700:4700::1111")]).is_ok());
    }

    #[test]
    fn url_validation_rejects_bad_schemes_and_credentials() {
        assert!(validate_public_http_url("ftp://example.com/x").is_err());
        assert!(validate_public_http_url("file:///C:/Windows").is_err());
        assert!(
            validate_public_http_url("https://user:pass@example.com/x").is_err(),
            "embedded credentials must be rejected"
        );
        assert!(validate_public_http_url("https://localhost/x").is_err());
        assert!(validate_public_http_url("http://127.0.0.1/x").is_err());
        assert!(validate_public_http_url("not a url").is_err());
    }

    #[test]
    fn html_to_text_strips_scripts_and_tags() {
        let html = r#"<html><head><title>Ignore me</title><style>body{color:red}</style></head>
            <body><!-- hidden comment --><script>alert(1)</script>
            <h1>Hello&nbsp;&amp;&#39;World</h1><p>First para.</p>
            <p>Second <b>bold</b> para.</p></body></html>"#;
        let text = html_to_text(html);
        assert!(!text.contains("alert"));
        assert!(!text.contains("body{"));
        assert!(!text.contains("hidden comment"));
        assert!(!text.to_lowercase().contains("<h1"));
        assert!(text.contains("Hello\u{00a0}&'World"));
        assert!(text.contains("First para."));
        assert!(text.contains("Second bold para."));
    }

    #[test]
    fn strip_tags_fragment_collapses_whitespace() {
        assert_eq!(
            strip_tags_fragment("<b>Rust</b>  &amp;\n  <i>Systems</i>"),
            "Rust & Systems"
        );
        assert_eq!(strip_tags_fragment("<!--c-->x"), "x");
    }

    #[test]
    fn ddg_links_decode_uddg_redirects() {
        assert_eq!(
            ddg_real_href("//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2Fa&rut=abc"),
            "https://example.com/a"
        );
        assert_eq!(
            ddg_real_href("https://plain.example.com"),
            "https://plain.example.com"
        );
    }

    #[test]
    fn parse_ddg_html_extracts_title_href_snippet() {
        let page = r#"<div class="result">
            <a rel="nofollow" class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fdocs.rs%2Fregex">regex crate</a>
            <a class="result__snippet">A <b>regex</b> library for Rust.</a>
            </div>"#;
        let results = parse_ddg_html(page);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "regex crate");
        assert_eq!(results[0].href, "https://docs.rs/regex");
        assert_eq!(results[0].snippet, "A regex library for Rust.");
    }

    #[test]
    fn deno_lockdown_uses_exact_bionic_flag_set() {
        let args = deno_lockdown_args("script.ts");
        for expected in [
            "--allow-read=.",
            "--allow-write=.",
            "--no-prompt",
            "--deny-net",
            "--deny-env",
            "--deny-sys",
            "--deny-run",
            "--deny-ffi",
        ] {
            assert!(args.iter().any(|a| a == expected), "missing {expected}");
        }
        // The script must come last, after every flag.
        assert_eq!(args.last().unwrap(), "script.ts");
        // No broad grants anywhere.
        assert!(!args.iter().any(|a| a == "--allow-all"));
    }

    #[test]
    fn render_code_result_shapes_success_and_failure() {
        let ok = render_code_result(
            "run_python",
            "Python script",
            true,
            "out\n".into(),
            String::new(),
            "python",
        );
        assert!(ok.success);
        assert_eq!(ok.error, None);
        let fail = render_code_result(
            "run_python",
            "Python script",
            false,
            "partial".into(),
            "Traceback…".into(),
            "python",
        );
        assert!(!fail.success);
        assert_eq!(fail.error.as_deref(), Some("Traceback…"));
        assert!(fail.stdout.unwrap().contains("--- stderr ---"));
    }
}
