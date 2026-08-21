//! Native tool implementations and the unified execution dispatcher.
//!
//! Every tool follows the same contract: `async fn … -> Result<ToolResult, String>`.
//! The dispatcher wraps them with real-time UI events and a `ToolResult`
//! envelope so the orchestrator gets a uniform response shape regardless of
//! whether the tool succeeded or failed.

use std::collections::{HashMap, HashSet};
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
    now_ms, plan, policy, AgentToolEvent, FileChangedEvent, PermissionDecision, PermissionRequestEvent,
    ToolCall, ToolResult, ToolState,
};

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

    emit(app, &AgentToolEvent {
        id: id.clone(),
        tool: tool.to_string(),
        status: "running".into(),
        summary: call.summary(),
        started_at,
        duration_ms: None,
        detail: None,
    });

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
        ToolCall::GlobSearchCodebase { pattern, root, respect_gitignore } => {
            glob_search_codebase(state, pattern, root.as_deref(), respect_gitignore.unwrap_or(true)).await
        }
        ToolCall::ViewFileStructure { path, max_depth } => {
            view_file_structure(path, max_depth.unwrap_or(4)).await
        }
        ToolCall::ReadFileRange { path, start_line, end_line } => {
            read_file_range(path, *start_line, *end_line).await
        }
        ToolCall::ApplyFileDiff { path, diff } => apply_file_diff(app, path, diff).await,
        ToolCall::WriteFile { path, content } => write_file(app, path, content).await,
        ToolCall::SearchFileContents { pattern, include, root, respect_gitignore } => {
            search_file_contents(
                state,
                pattern,
                include.as_deref(),
                root.as_deref(),
                respect_gitignore.unwrap_or(true),
            )
            .await
        }
        ToolCall::SemanticSearchCodebase { query, include, root, respect_gitignore, top_k } => {
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
        ToolCall::CreateSkill { name, description, content } => {
            create_skill(app, state, name, description.as_deref(), content).await
        }
        ToolCall::ReadSkill { name } => read_skill(state, name).await,
        ToolCall::ExecuteTerminalCommand { command, timeout_secs, cwd } => {
            execute_terminal_command(app, state, &interrupt, command, *timeout_secs, cwd.as_deref()).await
        }
        ToolCall::CallMcpTool { server_bin, server_args, tool, arguments, timeout_secs } => {
            call_mcp_tool(state, &interrupt, server_bin, server_args, tool, arguments, *timeout_secs).await
        }
        ToolCall::GitStatus { .. } => git_capture(state, &interrupt, &["status", "--short", "--branch"], None).await,
        ToolCall::GitDiff { path } => git_diff(state, &interrupt, path.as_deref()).await,
        ToolCall::GitCommit { message } => git_commit(state, &interrupt, message).await,
        ToolCall::GitCheckpoint { message } => git_checkpoint(state, &interrupt, message.as_deref()).await,
        ToolCall::GitRevert { commit } => git_revert(state, &interrupt, commit.as_deref()).await,
        ToolCall::RunTests { command } => run_tests(app, state, &interrupt, command.as_deref()).await,
        ToolCall::CreatePlan { title, goal, items } => create_plan(state, title, goal, items).await,
        ToolCall::ReadPlan {} => read_plan(state).await,
        ToolCall::UpdatePlan { item, status, details } => update_plan(state, *item, status, details.as_deref()).await,
        // ExecutePlan is intercepted by the orchestrator before dispatch; treat as unreachable.
        ToolCall::ExecutePlan {} => Ok(ToolResult::err("execute_plan", "execute_plan is handled by the orchestrator".into(), "should not reach dispatch".into())),
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
            status: if final_result.success { "done".into() } else { "error".into() },
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
/// logged — raw args (file content, secrets) are never written.
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
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
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
    let (tx, rx) = tokio::sync::oneshot::channel();
    {
        let mut reqs = state.permission_requests.lock().await;
        reqs.insert(request_id.to_string(), tx);
    }
    let _ = app.emit(
        "agent://permission-request",
        PermissionRequestEvent {
            request_id: request_id.to_string(),
            tool: tool.to_string(),
            summary,
            timestamp_ms: now_ms(),
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
        Rcvd::Decision(PermissionDecision::AllowOnce) => AskOutcome::GrantedOnce,
        Rcvd::Decision(PermissionDecision::AllowSession) => AskOutcome::GrantedSession,
        Rcvd::Decision(PermissionDecision::AlwaysAllow) => AskOutcome::GrantedAlways,
        Rcvd::Decision(PermissionDecision::Deny) => AskOutcome::Declined,
        Rcvd::TimedOut => AskOutcome::TimedOut,
        Rcvd::Aborted => AskOutcome::Aborted,
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
    for hunk in diff.unified_diff().context_radius(2).header(path, path).iter_hunks() {
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
}

async fn resolve_root(state: &ToolState, root: Option<&str>) -> Result<PathBuf, String> {
    if let Some(r) = root {
        let p = PathBuf::from(r);
        if p.is_dir() {
            return Ok(p);
        }
        return Err(format!("Root path does not exist or is not a directory: {r}"));
    }
    let guard = state.workspace.lock().await;
    match guard.as_ref() {
        Some(p) => Ok(p.clone()),
        None => Err(
            "No workspace set yet - open a workspace first, or pass an explicit `root`.".to_string(),
        ),
    }
}

fn rel_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root).unwrap_or(path).to_string_lossy().replace('\\', "/")
}

// ---------------------------------------------------------------------------
// glob_search_codebase
// ---------------------------------------------------------------------------

const SKIP_DIRS: &[&str] = &[
    "node_modules", ".git", "target", "dist", "build", ".next", ".venv", "venv",
    ".cache", "vendor", "__pycache__",
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
        let basename = path.file_name().map(|f| f.to_string_lossy()).unwrap_or_default();
        if matcher.is_match(&rel_path(&root, path)) || matcher.is_match(basename.as_ref()) {
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
        let Ok(meta) = std::fs::metadata(path) else { continue };
        if meta.len() > MAX_SEARCH_FILE_SIZE {
            continue;
        }
        files_searched += 1;

        let Ok(file) = tokio::fs::File::open(path).await else { continue };
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
    "the", "and", "are", "for", "not", "but", "with", "this", "that", "from", "have",
    "has", "was", "were", "you", "your", "will", "into", "than", "then", "them", "their",
    "been", "being", "about", "would", "could", "should", "there", "here", "when",
    "where", "which", "while", "after", "before", "also", "over", "under", "each",
    "between", "within", "above", "such", "only", "very", "just", "can", "make",
    "make", "used", "use", "using", "does", "doing", "done", "does", "was",
    "its", "it's", "our", "out", "all", "any", "both", "few", "more", "most",
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
            if i > 0 && c.is_uppercase() && !current.is_empty()
                && (chars[i - 1].is_lowercase() || chars[i - 1].is_ascii_digit())
            {
                if !current.is_empty() {
                    out.push(current.to_lowercase());
                    current.clear();
                }
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
        let Ok(meta) = std::fs::metadata(path) else { continue };
        if meta.len() > MAX_SEARCH_FILE_SIZE {
            continue;
        }
        files_indexed += 1;

        let Ok(bytes) = tokio::fs::read(path).await else { continue };
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
    let mut rank = 1usize;
    for (score, c) in scored.iter().take(k) {
        out.push_str(&format!(
            "{rank:>2}. {:.2}  {}:{}:{}\n",
            score,
            c.path,
            c.start_line,
            c.start_line + WINDOW_LINES - 1
        ));
        rank += 1;
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
    let lang: Language = tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into();    let mut parser = Parser::new();
    parser
        .set_language(&lang)
        .map_err(|e| format!("Parser language error: {e}"))?;
    let tree = parser
        .parse(&src, None)
        .ok_or_else(|| format!("Failed to parse `{path}`"))?;

    let mut defs: Vec<Def> = Vec::new();
    let mut seen: HashSet<(String, String, usize)> = HashSet::new();
    collect_defs(tree.root_node(), &src, 0, max_depth, &mut defs, &mut seen);
    defs.sort_by(|a, b| a.line.cmp(&b.line));
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
            &sig.chars().take(120).collect::<String>(),
        ));
        out.push('\n');
    }

    let summary = format!(
        "Parsed `{}` - {} top-level declaration(s)",
        Path::new(path).file_name().map(|f| f.to_string_lossy()).unwrap_or_default(),
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
            out.push(Def { kind, name, sig, line: start_line, end_line });
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
    let name = Path::new(path).file_name().map(|f| f.to_string_lossy()).unwrap_or_default();
    let summary = if truncated {
        format!("Read `{name}` lines {start}..={end} of {total} (truncated to {MAX_READ_LINES})")
    } else {
        format!("Read `{name}` lines {start}..={end} of {total}")
    };

    Ok(ToolResult::ok(
        "read_file_range",
        summary,
        Some(snippet),
        Some(json!({ "totalLines": total, "readFrom": start, "readTo": end, "truncated": truncated, "bytes": bytes.len() })),
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
        return Err("SEARCH block is empty - include the exact existing lines to replace".to_string());
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
        let mut out = Vec::with_capacity(
            file_lines.len() - search_lines.len() + replace_lines.len() + 1,
        );
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
    let tmp = PathBuf::from(format!("{}.{}.{nanos}.agent-tmp", p.display(), std::process::id()));
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
            pump(&app_for_output, "stdout", BufReader::new(stdout).lines(), &mut so, MAX_CMD_OUTPUT as usize),
            pump(&app_for_output, "stderr", BufReader::new(stderr).lines(), &mut se, MAX_CMD_OUTPUT as usize),
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
            Ok(ToolResult::err("execute_terminal_command", msg.clone(), msg))
        }
        CmdOutcome::Finished(status, so, se) => {
            let status = match status {
                Ok(s) => s,
                Err(e) => return Err(format!("Command failed to run: {e}")),
            };
            let code = status.code();
            let success = status.success();
            let elapsed = started.elapsed().as_millis() as u64;
            let combined = if se.is_empty() { so.clone() } else { format!("{so}\n{se}") };
            let summary = if success {
                format!("Command succeeded (exit {}) in {elapsed}ms", code.unwrap_or(0))
            } else {
                format!("Command failed (exit {}) in {elapsed}ms", code.unwrap_or(-1))
            };
            Ok(ToolResult {
                success,
                tool: "execute_terminal_command".to_string(),
                summary,
                stdout: Some(combined),
                error: if success { None } else { Some(format!("exit code {}", code.unwrap_or(-1))) },
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

async fn call_mcp_tool(
    state: &ToolState,
    interrupt: &CancellationToken,
    server_bin: &str,
    server_args: &[String],
    tool: &str,
    arguments: &Value,
    timeout_secs: Option<u64>,
) -> Result<ToolResult, String> {
    let timeout = std::time::Duration::from_secs(timeout_secs.unwrap_or(30).clamp(1, 300));
    let key = format!("{server_bin} {}", server_args.join(" "));

    let handle = {
        let mut servers = state.mcp_servers.lock().await;
        if let Some(h) = servers.get(&key) {
            h.clone()
        } else {
            let h = std::sync::Arc::new(tokio::sync::Mutex::new(
                super::mcp::McpHandle::spawn(server_bin, server_args).await?,
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
        format!("MCP `{tool}` returned {} character(s)", text.chars().count()),
        Some(text),
        Some(json!({ "mcpServer": key, "tool": tool })),
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
    if cleaned.is_empty() { "skill".into() } else { cleaned }
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

    let _ = app.emit("agent://skills-changed", json!({ "name": name, "path": path.to_string_lossy() }));
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
    out.push_str(&skill.content.trim());
    out.push('\n');
    Ok(ToolResult::ok(
        "read_skill",
        format!("Loaded skill `{}` ({} chars)", skill.name, skill.content.len()),
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
    let result = execute_terminal_command(app, state, interrupt, &command, Some(180), Some(dir.to_str().unwrap_or("."))).await?;
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
        error: if ok { None } else { Some("Test suite reported failures".into()) },
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
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("Failed to run `git {}`: {e} (is git on PATH?)", full_args.join(" ")))?;
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
    let combined = if se.is_empty() { so.clone() } else { format!("{so}\n{se}") };
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

async fn git_diff(state: &ToolState, interrupt: &CancellationToken, path: Option<&str>) -> Result<ToolResult, String> {
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
        if path.is_some() { "for the requested path" } else { "(full)" }
    );
    Ok(body)
}

async fn git_commit(state: &ToolState, interrupt: &CancellationToken, message: &str) -> Result<ToolResult, String> {
    if message.trim().is_empty() {
        return Err("git_commit needs a non-empty message".into());
    }
    git_capture(state, interrupt, &["add", "-A"], None).await?;
    git_capture(state, interrupt, &["commit", "-m", message], None).await
}

/// Save a checkpoint: a real commit tagged with a `checkpoint:` prefix so
/// `git_revert` can find it later.
pub async fn git_checkpoint(state: &ToolState, interrupt: &CancellationToken, message: Option<&str>) -> Result<ToolResult, String> {
    let msg = match message {
        Some(m) if !m.trim().is_empty() => m.to_string(),
        _ => format!("checkpoint: {}", now_ms()),
    };
    let msg = if msg.starts_with("checkpoint:") { msg } else { format!("checkpoint: {msg}") };
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
        &["log", "--grep=^checkpoint:", "--format=%H%x1f%s%x1f%ct", "-n", "20"],
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
            let (Some(hash), Some(subject), Some(ts)) = (parts.next(), parts.next(), parts.next()) else {
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
                    let since = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
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
pub async fn git_revert(state: &ToolState, interrupt: &CancellationToken, commit: Option<&str>) -> Result<ToolResult, String> {
    let target = match commit {
        Some(c) if !c.trim().is_empty() => c.to_string(),
        _ => {
            let head = git_capture(state, interrupt, &["log", "--grep=^checkpoint:", "--format=%H", "-1"], None).await?;
            match head.stdout.and_then(|s| s.trim().split_whitespace().next().map(str::to_string)) {
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
        format!("Plan `{}` created with {} items. See `.ai/plan.md`.", plan.title, plan.items.len()),
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
        format!("Plan `{}` — {}/{} items completed.", plan.title, plan.items.iter().filter(|i| i.status == plan::PlanStatus::Completed).count(), plan.items.len()),
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
    let new_status = plan::PlanStatus::from_label(status)
        .ok_or_else(|| format!("Unknown status `{status}`. Use: not_started, in_progress, completed, terminal."))?;
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
        format!("Updated plan item #{item} `{title}` → {}.", new_status.label()),
        None,
        None,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(toks.contains(&"authlogin".to_string()) == false);
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
}
