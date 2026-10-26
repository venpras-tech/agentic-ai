//! Agentic core: tool-call execution engine for the local coding assistant.
//!
//! Modules:
//!   * [`tools`]       - the native tool implementations + unified dispatcher
//!   * [`core`]        - Markdown `<execute_tool>` tag parser + JSON schemas
//!   * [`mcp`]         - minimal stdio JSON-RPC MCP (Model Context Protocol) client
//!   * [`orchestrator`]- generate → parse → dispatch → feedback loop
//!   * [`context`]     - token tracking + sliding-window eviction engine
//!   * [`policy`]      - per-tool allow/ask/deny + red-zone + workspace scoping
//!   * [`skills`]      - project rules + toggleable skill bundles
//!
//! Design goals:
//!   * Zero SaaS dependencies - every tool runs on-device.
//!   * Safe error propagation - tools return `Result<ToolResult, String>` and
//!     the dispatcher flattens failures into a `ToolResult{success:false}` so
//!     the agent can never crash the app.
//!   * UI transparency - every tool run emits start/done events that the
//!     React timeline renders in real time.

pub mod background;
pub mod context;
pub mod core;
pub mod edits;
pub mod hooks;
pub mod interrupt;
pub mod mcp;
pub mod orchestrator;
pub mod paths;
pub mod plan;
pub mod policy;
pub mod prompt;
pub mod rag;
pub mod registry;
pub mod skills;
pub mod subagent;
pub mod todo;
pub mod tools;
pub mod verify;
pub mod workflows;

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

/// Abstraction over the host application's event bus so the [`agent`] layer
/// stays free of `tauri`/`AppHandle`. A [`TauriEventSink`](crate::commands::agent::TauriEventSink)
/// adapter wires this to Tauri's `Emitter` in the command layer.
///
/// Each `emit` corresponds to a real-time `agent://*` event the frontend
/// listens for. Payloads arrive already serialized as JSON; agent code uses
/// [`ToolContext::emit`] (typed) instead of talking to the feature directly.
pub trait EventSink: Send + Sync {
    fn emit_value(&self, event: &str, payload: &serde_json::Value);
}

/// Minimal, tauri-free context the tool layer needs to talk to the host:
/// the event bus plus a few app-owned paths (currently just the config
/// directory used for MCP catalog + policy rules). Injected at the command
/// boundary from the real `AppHandle`; `Clone` so worker threads can move it.
#[derive(Clone)]
pub struct ToolContext {
    /// Bridges `agent://*` events to the host UI.
    pub sink: std::sync::Arc<dyn EventSink>,
    /// Absolute config directory (MCP catalog, policy, plan files).
    pub config_dir: PathBuf,
}

impl ToolContext {
    /// Serialize and emit a typed [`agent://*`](EventSink) event. Thin wrapper
    /// over [`EventSink::emit_value`] so call sites never touch JSON directly.
    pub fn emit(&self, event: &str, payload: impl Serialize) {
        if let Ok(value) = serde_json::to_value(payload) {
            self.sink.emit_value(event, &value);
        }
    }
}

/// A structured tool invocation, serialized as
/// `{ "type": "<tool_name>", ...params }` (snake_case type tag, camelCase fields).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolCall {
    #[serde(rename_all = "camelCase")]
    GlobSearchCodebase {
        pattern: String,
        root: Option<String>,
        #[serde(default)]
        respect_gitignore: Option<bool>,
    },
    #[serde(rename_all = "camelCase")]
    SearchFileContents {
        pattern: String,
        #[serde(default)]
        include: Option<String>,
        root: Option<String>,
        #[serde(default)]
        respect_gitignore: Option<bool>,
    },
    #[serde(rename_all = "camelCase")]
    ViewFileStructure {
        path: String,
        #[serde(default)]
        max_depth: Option<usize>,
    },
    #[serde(rename_all = "camelCase")]
    ReadFileRange {
        path: String,
        start_line: u64,
        end_line: u64,
    },
    #[serde(rename_all = "camelCase")]
    ApplyFileDiff { path: String, diff: String },
    #[serde(rename_all = "camelCase")]
    ExecuteTerminalCommand {
        command: String,
        #[serde(default)]
        timeout_secs: Option<u64>,
        #[serde(default)]
        cwd: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    CallMcpTool {
        /// Catalog name of a configured server (preferred).
        #[serde(default)]
        server: Option<String>,
        /// Ad-hoc executable path (legacy/ad-hoc calls without a catalog entry).
        #[serde(default)]
        server_bin: Option<String>,
        #[serde(default)]
        server_args: Vec<String>,
        tool: String,
        #[serde(default)]
        arguments: serde_json::Value,
        #[serde(default)]
        timeout_secs: Option<u64>,
    },
    #[serde(rename_all = "camelCase")]
    ListMcpServers {},
    #[serde(rename_all = "camelCase")]
    AddMcpServer {
        name: String,
        bin: String,
        #[serde(default)]
        args: Vec<String>,
    },
    #[serde(rename_all = "camelCase")]
    RemoveMcpServer { name: String },
    #[serde(rename_all = "camelCase")]
    AttachFile { path: String },
    #[serde(rename_all = "camelCase")]
    SearchAttachedFiles {
        query: String,
        #[serde(default)]
        top_k: Option<usize>,
    },
    #[serde(rename_all = "camelCase")]
    DetachFile { path: String },
    #[serde(rename_all = "camelCase")]
    TranscribeAudio {
        path: String,
        #[serde(default)]
        language: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    GitStatus {},
    #[serde(rename_all = "camelCase")]
    GitDiff { path: Option<String> },
    #[serde(rename_all = "camelCase")]
    GitCommit { message: String },
    #[serde(rename_all = "camelCase")]
    GitCheckpoint {
        #[serde(default)]
        message: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    GitRevert {
        #[serde(default)]
        commit: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    RunTests {
        #[serde(default)]
        command: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    WriteFile { path: String, content: String },
    #[serde(rename_all = "camelCase")]
    CreateSkill {
        name: String,
        #[serde(default)]
        description: Option<String>,
        content: String,
    },
    #[serde(rename_all = "camelCase")]
    ReadSkill { name: String },
    #[serde(rename_all = "camelCase")]
    SuggestSkills {
        #[serde(default)]
        prompt: String,
        #[serde(default)]
        path: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    SemanticSearchCodebase {
        query: String,
        #[serde(default)]
        include: Option<String>,
        root: Option<String>,
        #[serde(default)]
        respect_gitignore: Option<bool>,
        #[serde(default)]
        top_k: Option<usize>,
    },
    #[serde(rename_all = "camelCase")]
    CreatePlan {
        title: String,
        #[serde(default)]
        goal: String,
        items: Vec<String>,
    },
    #[serde(rename_all = "camelCase")]
    ReadPlan {},
    #[serde(rename_all = "camelCase")]
    UpdatePlan {
        /// 1-based plan item index.
        item: usize,
        /// "not_started" | "in_progress" | "completed" | "terminal".
        status: String,
        #[serde(default)]
        details: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    ExecutePlan {},
    #[serde(rename_all = "camelCase")]
    ListDir {
        #[serde(default)]
        path: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    ReadFileChars {
        path: String,
        #[serde(default)]
        offset: Option<usize>,
        #[serde(default)]
        limit: Option<usize>,
    },
    #[serde(rename_all = "camelCase")]
    CreateFolder { path: String },
    #[serde(rename_all = "camelCase")]
    CopyFileOrFolder {
        src: String,
        dst: String,
        #[serde(default)]
        can_overwrite: Option<bool>,
    },
    #[serde(rename_all = "camelCase")]
    MoveFileOrFolder {
        src: String,
        dst: String,
        #[serde(default)]
        can_overwrite: Option<bool>,
    },
    #[serde(rename_all = "camelCase")]
    DeleteFileOrFolder { path: String },
    #[serde(rename_all = "camelCase")]
    GetScratchpadFolder {},
    #[serde(rename_all = "camelCase")]
    SetTodoList { items: Vec<String> },
    #[serde(rename_all = "camelCase")]
    GetTodoList {},
    #[serde(rename_all = "camelCase")]
    MarkTodoItemDone {
        /// 1-based todo index.
        item: usize,
    },
    #[serde(rename_all = "camelCase")]
    WebSearch {
        query: String,
        #[serde(default)]
        max_results: Option<usize>,
    },
    #[serde(rename_all = "camelCase")]
    WebExtract { url: String },
    #[serde(rename_all = "camelCase")]
    DownloadFile { url: String, path: String },
    #[serde(rename_all = "camelCase")]
    RunPython {
        code: String,
        #[serde(default)]
        timeout_secs: Option<u64>,
    },
    #[serde(rename_all = "camelCase")]
    RunJavascript {
        code: String,
        #[serde(default)]
        timeout_secs: Option<u64>,
    },
    /// Deterministic arithmetic evaluation — no subprocess, no approval.
    /// Lets small models answer math reliably instead of guessing.
    #[serde(rename_all = "camelCase")]
    Calculate { expression: String },
    /// First-class subagent (P1-8): delegate one focused piece of work to a
    /// named specialist profile that runs its own tool loop on a spare engine
    /// worker and reports distilled findings back. Synchronous — the call
    /// returns when the child finishes (`subagent_await` is implicit).
    #[serde(rename_all = "camelCase")]
    Task {
        /// Specialist profile: "explore" | "implement" | "review".
        #[serde(default)]
        subagent_type: Option<String>,
        /// Self-contained instruction for the child agent.
        task: String,
        /// Optional model override (GGUF path or "remote" for current remote
        /// config). When set, the subagent runs on a different model than the
        /// parent — enabling architect mode (large planner + small editor).
        #[serde(default)]
        model_override: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    GitBlame {
        path: String,
        #[serde(default)]
        start_line: Option<u64>,
        #[serde(default)]
        end_line: Option<u64>,
    },
    #[serde(rename_all = "camelCase")]
    GitPush {
        #[serde(default)]
        remote: Option<String>,
        #[serde(default)]
        branch: Option<String>,
        /// Push with `-u` to set upstream tracking (first push of a branch).
        #[serde(default)]
        set_upstream: Option<bool>,
    },
    #[serde(rename_all = "camelCase")]
    GitPull {},
    /// Create a branch AND switch to it (`git switch -c`) so subsequent
    /// commits/pushes land on it. Refuses when the branch already exists.
    #[serde(rename_all = "camelCase")]
    GitCreateBranch { name: String },
    #[serde(rename_all = "camelCase")]
    GitPrStatus {},
    #[serde(rename_all = "camelCase")]
    GitCiStatus {},
    #[serde(rename_all = "camelCase")]
    CreatePr {
        title: String,
        #[serde(default)]
        body: Option<String>,
    },
    /// Produce a concise NL summary of uncommitted changes (diff + status).
    #[serde(rename_all = "camelCase")]
    SummarizeChanges {},
    /// Lightweight single-file lint pass (P1-11): tree-sitter syntax errors
    /// for JS/TS, TODO/FIXME/HACK/XXX comment markers for any file, plus
    /// cheap AST checks (empty catch blocks, stray `debugger` statements).
    #[serde(rename_all = "camelCase")]
    ReadLints { path: String },
    /// Ask the user a blocking question with optional preset choices.
    /// Returns once the user answers (or the request times out).
    #[serde(rename_all = "camelCase")]
    AskQuestion {
        question: String,
        #[serde(default)]
        choices: Option<Vec<String>>,
    },
    /// Fire-and-forget message addressed to the human user (not to the model).
    #[serde(rename_all = "camelCase")]
    SendToUser { message: String },
    /// Run a tree-sitter S-expression query against a source file and return
    /// all matches with capture names, node text, and positions.
    #[serde(rename_all = "camelCase")]
    TreeSitterQuery {
        path: String,
        query: String,
        #[serde(default)]
        max_results: Option<usize>,
    },
    /// Diagnose a bug from a stack trace / error description. Parses
    /// `file:line` references, reads the surrounding code, searches the
    /// workspace for related definitions, and returns a structured report of
    /// the suspected root cause with fix suggestions.
    #[serde(rename_all = "camelCase")]
    AnalyzeBug {
        /// Stack trace (or free-form error description) to analyze.
        stack: String,
        /// Optional file to focus on even if no `file:line` refs parse.
        #[serde(default)]
        path: Option<String>,
    },
    /// Read-only code review: analyze a file path, an inline diff, or the
    /// current uncommitted changes for bugs, style issues and security
    /// concerns. Returns structured findings (severity, location, suggestion).
    #[serde(rename_all = "camelCase")]
    ReviewCode {
        /// Absolute or workspace-relative file path to review.
        #[serde(default)]
        path: Option<String>,
        /// Unified diff text to review instead of a file.
        #[serde(default)]
        diff: Option<String>,
    },
    /// Deterministic repo map: extract symbol definitions + references from
    /// source files, build a directed reference graph, run iterative PageRank,
    /// and return the top-ranked symbols within a context budget.
    #[serde(rename_all = "camelCase")]
    ViewRepoMap {
        /// Maximum number of symbols to return (default 60).
        #[serde(default)]
        top_n: Option<usize>,
        /// Workspace root override.
        #[serde(default)]
        root: Option<String>,
    },
    /// Headless web fetch: retrieve a URL's text content via HTTP GET,
    /// validated against the SSRF guard.
    #[serde(rename_all = "camelCase")]
    BrowseWeb {
        /// URL to fetch.
        url: String,
        /// Action: "fetch" (GET + text extraction) or "screenshot".
        #[serde(default)]
        action: Option<String>,
    },
}

impl ToolCall {
    pub fn name(&self) -> &'static str {
        match self {
            ToolCall::GlobSearchCodebase { .. } => "glob_search_codebase",
            ToolCall::SearchFileContents { .. } => "search_file_contents",
            ToolCall::ViewFileStructure { .. } => "view_file_structure",
            ToolCall::ReadFileRange { .. } => "read_file_range",
            ToolCall::ApplyFileDiff { .. } => "apply_file_diff",
            ToolCall::ExecuteTerminalCommand { .. } => "execute_terminal_command",
            ToolCall::CallMcpTool { .. } => "call_mcp_tool",
            ToolCall::GitStatus { .. } => "git_status",
            ToolCall::GitDiff { .. } => "git_diff",
            ToolCall::GitCommit { .. } => "git_commit",
            ToolCall::GitCheckpoint { .. } => "git_checkpoint",
            ToolCall::GitRevert { .. } => "git_revert",
            ToolCall::RunTests { .. } => "run_tests",
            ToolCall::WriteFile { .. } => "write_file",
            ToolCall::CreateSkill { .. } => "create_skill",
            ToolCall::ReadSkill { .. } => "read_skill",
            ToolCall::SuggestSkills { .. } => "suggest_skills",
            ToolCall::SemanticSearchCodebase { .. } => "semantic_search_codebase",
            ToolCall::CreatePlan { .. } => "create_plan",
            ToolCall::ReadPlan { .. } => "read_plan",
            ToolCall::UpdatePlan { .. } => "update_plan",
            ToolCall::ExecutePlan { .. } => "execute_plan",
            ToolCall::ListDir { .. } => "list_dir",
            ToolCall::ReadFileChars { .. } => "read_file_chars",
            ToolCall::CreateFolder { .. } => "create_folder",
            ToolCall::CopyFileOrFolder { .. } => "copy_file_or_folder",
            ToolCall::MoveFileOrFolder { .. } => "move_file_or_folder",
            ToolCall::DeleteFileOrFolder { .. } => "delete_file_or_folder",
            ToolCall::GetScratchpadFolder { .. } => "get_scratchpad_folder",
            ToolCall::SetTodoList { .. } => "set_todo_list",
            ToolCall::GetTodoList { .. } => "get_todo_list",
            ToolCall::MarkTodoItemDone { .. } => "mark_todo_item_done",
            ToolCall::WebSearch { .. } => "web_search",
            ToolCall::WebExtract { .. } => "web_extract",
            ToolCall::DownloadFile { .. } => "download_file",
            ToolCall::RunPython { .. } => "run_python",
            ToolCall::RunJavascript { .. } => "run_javascript",
            ToolCall::Calculate { .. } => "calculate",
            ToolCall::Task { .. } => "task",
            ToolCall::GitBlame { .. } => "git_blame",
            ToolCall::GitPush { .. } => "git_push",
            ToolCall::GitPull { .. } => "git_pull",
            ToolCall::GitCreateBranch { .. } => "git_create_branch",
            ToolCall::GitPrStatus { .. } => "git_pr_status",
            ToolCall::GitCiStatus { .. } => "git_ci_status",
            ToolCall::CreatePr { .. } => "create_pr",
            ToolCall::SummarizeChanges { .. } => "summarize_changes",
            ToolCall::ReadLints { .. } => "read_lints",
            ToolCall::AskQuestion { .. } => "ask_question",
            ToolCall::SendToUser { .. } => "send_to_user",
            ToolCall::ListMcpServers { .. } => "list_mcp_servers",
            ToolCall::AddMcpServer { .. } => "add_mcp_server",
            ToolCall::RemoveMcpServer { .. } => "remove_mcp_server",
            ToolCall::AttachFile { .. } => "attach_file",
            ToolCall::SearchAttachedFiles { .. } => "search_attached_files",
            ToolCall::DetachFile { .. } => "detach_file",
            ToolCall::TranscribeAudio { .. } => "transcribe_audio",
            ToolCall::TreeSitterQuery { .. } => "tree_sitter_query",
            ToolCall::AnalyzeBug { .. } => "analyze_bug",
            ToolCall::ReviewCode { .. } => "review_code",
            ToolCall::ViewRepoMap { .. } => "view_repo_map",
            ToolCall::BrowseWeb { .. } => "browse_web",
        }
    }

    /// Every snake_case tool name the dispatcher can execute.
    ///
    /// Mirrors the exhaustive arms of [`ToolCall::name`] as a plain list so the
    /// canonical [`registry`](crate::agent::registry) can be drift-checked
    /// against the dispatcher (`core::test_registry_matches_dispatcher`). Add a
    /// name here whenever a new `ToolCall` variant ships.
    #[allow(dead_code)]
    pub fn all_tool_names() -> Vec<&'static str> {
        vec![
            "glob_search_codebase",
            "search_file_contents",
            "view_file_structure",
            "read_file_range",
            "apply_file_diff",
            "execute_terminal_command",
            "call_mcp_tool",
            "git_status",
            "git_diff",
            "git_commit",
            "git_checkpoint",
            "git_revert",
            "run_tests",
            "write_file",
            "create_skill",
            "read_skill",
            "suggest_skills",
            "semantic_search_codebase",
            "create_plan",
            "read_plan",
            "update_plan",
            "execute_plan",
            "list_dir",
            "read_file_chars",
            "create_folder",
            "copy_file_or_folder",
            "move_file_or_folder",
            "delete_file_or_folder",
            "get_scratchpad_folder",
            "set_todo_list",
            "get_todo_list",
            "mark_todo_item_done",
            "web_search",
            "web_extract",
            "download_file",
            "run_python",
            "run_javascript",
            "calculate",
            "task",
            "git_blame",
            "git_push",
            "git_pull",
            "git_create_branch",
            "git_pr_status",
            "git_ci_status",
            "create_pr",
            "summarize_changes",
            "read_lints",
            "ask_question",
            "send_to_user",
            "list_mcp_servers",
            "add_mcp_server",
            "remove_mcp_server",
            "attach_file",
            "search_attached_files",
            "detach_file",
            "transcribe_audio",
            "tree_sitter_query",
            "analyze_bug",
            "review_code",
            "view_repo_map",
            "browse_web",
        ]
    }

    /// A short human-readable description used in the UI timeline header.
    pub fn summary(&self) -> String {
        match self {
            ToolCall::GlobSearchCodebase { pattern, .. } => {
                format!("Scanning workspace for `{pattern}`…")
            }
            ToolCall::SearchFileContents {
                pattern, include, ..
            } => match include {
                Some(inc) if !inc.is_empty() => {
                    format!("Searching `{inc}` for `{pattern}`…")
                }
                _ => format!("Searching workspace for `{pattern}`…"),
            },
            ToolCall::ViewFileStructure { path, .. } => {
                format!("Parsing AST of `{}`…", display_name(path))
            }
            ToolCall::ReadFileRange {
                path,
                start_line,
                end_line,
            } => {
                format!(
                    "Reading `{}` lines {start_line}..={end_line}…",
                    display_name(path)
                )
            }
            ToolCall::ApplyFileDiff { path, .. } => {
                format!("Applying edit to `{}`…", display_name(path))
            }
            ToolCall::ExecuteTerminalCommand { command, .. } => {
                format!("Executing `{command}`…")
            }
            ToolCall::CallMcpTool { tool, .. } => format!("Calling MCP tool `{tool}`…"),
            ToolCall::GitStatus { .. } => "Reading git status…".into(),
            ToolCall::GitDiff {
                path: Some(path), ..
            } => {
                format!("Showing git diff for `{}`…", display_name(path))
            }
            ToolCall::GitDiff { path: None, .. } => "Showing git diff…".into(),
            ToolCall::GitCommit { .. } => "Committing changes…".into(),
            ToolCall::GitCheckpoint { .. } => "Creating a git checkpoint…".into(),
            ToolCall::GitRevert { .. } => "Reverting to a checkpoint…".into(),
            ToolCall::GitBlame {
                path,
                start_line,
                end_line,
            } => match (start_line, end_line) {
                (Some(s), Some(e)) => {
                    format!("Blaming `{}` lines {s}..={e}…", display_name(path))
                }
                _ => format!("Blaming `{}`…", display_name(path)),
            },
            ToolCall::GitPush { .. } => "Pushing commits to the remote…".into(),
            ToolCall::GitPull { .. } => "Pulling from the remote…".into(),
            ToolCall::GitCreateBranch { name } => {
                format!("Creating and switching to branch `{name}`…")
            }
            ToolCall::GitPrStatus { .. } => "Checking pull-request status…".into(),
            ToolCall::GitCiStatus { .. } => "Checking recent CI runs…".into(),
            ToolCall::CreatePr { title, .. } => {
                let short: String = title.chars().take(60).collect();
                format!("Opening pull request \"{short}\"…")
            }
            ToolCall::SummarizeChanges { .. } => "Summarizing uncommitted changes…".into(),
            ToolCall::ReadLints { path } => {
                format!("Linting `{}`…", display_name(path))
            }
            ToolCall::AskQuestion { question, .. } => {
                let short: String = question
                    .lines()
                    .next()
                    .unwrap_or("Question")
                    .chars()
                    .take(80)
                    .collect();
                format!("Asking the user: {short}…")
            }
            ToolCall::SendToUser { message } => {
                let short: String = message
                    .lines()
                    .next()
                    .unwrap_or("")
                    .chars()
                    .take(80)
                    .collect();
                format!("Message to user: {short}")
            }
            ToolCall::RunTests { .. } => "Running the test suite…".into(),
            ToolCall::WriteFile { path, .. } => {
                format!("Writing `{}`…", display_name(path))
            }
            ToolCall::CreateSkill { name, .. } => format!("Learning skill `{name}`…"),
            ToolCall::ReadSkill { name, .. } => format!("Loading skill `{name}`…"),
            ToolCall::SuggestSkills { prompt, .. } => {
                let p = if prompt.chars().count() > 60 {
                    format!("{}…", prompt.chars().take(60).collect::<String>())
                } else {
                    prompt.clone()
                };
                format!("Suggesting skills for: {p}…")
            }
            ToolCall::SemanticSearchCodebase { query, .. } => {
                format!("Semantic search: `{query}`…")
            }
            ToolCall::CreatePlan { title, items, .. } => {
                format!("Creating plan `{title}` ({} items)…", items.len())
            }
            ToolCall::ReadPlan { .. } => "Reading the active plan…".into(),
            ToolCall::UpdatePlan { item, status, .. } => {
                format!("Updating plan item #{item} → {status}…")
            }
            ToolCall::ExecutePlan { .. } => "Executing the approved plan…".into(),
            ToolCall::ListDir { path } => match path {
                Some(p) => format!("Listing `{}`…", display_name(p)),
                None => "Listing the workspace root…".into(),
            },
            ToolCall::ReadFileChars { path, offset, .. } => {
                format!(
                    "Reading `{}` from char {}…",
                    display_name(path),
                    offset.unwrap_or(0)
                )
            }
            ToolCall::CreateFolder { path } => format!("Creating folder `{path}`…"),
            ToolCall::CopyFileOrFolder { src, dst, .. } => {
                format!("Copying `{}` → `{}`…", display_name(src), display_name(dst))
            }
            ToolCall::MoveFileOrFolder { src, dst, .. } => {
                format!("Moving `{}` → `{}`…", display_name(src), display_name(dst))
            }
            ToolCall::DeleteFileOrFolder { path } => {
                format!("Deleting `{}` (moves to the OS Trash)…", display_name(path))
            }
            ToolCall::GetScratchpadFolder { .. } => {
                "Resolving the per-session scratchpad folder…".into()
            }
            ToolCall::SetTodoList { items } => {
                format!("Setting the todo list ({} item(s))…", items.len())
            }
            ToolCall::GetTodoList { .. } => "Reading the todo list…".into(),
            ToolCall::MarkTodoItemDone { item } => {
                format!("Marking todo #{item} done…")
            }
            ToolCall::WebSearch { query, .. } => {
                format!("Searching the web for `{query}`…")
            }
            ToolCall::WebExtract { url } => {
                format!("Fetching `{url}`…")
            }
            ToolCall::DownloadFile { url, path } => {
                format!("Downloading `{url}` → `{}`…", display_name(path))
            }
            ToolCall::RunPython { .. } => "Running sandboxed Python…".into(),
            ToolCall::RunJavascript { .. } => "Running sandboxed JavaScript…".into(),
            ToolCall::Calculate { expression } => format!("Calculating `{expression}`…"),
            ToolCall::Task {
                subagent_type,
                task,
                model_override: _,
            } => {
                let short: String = task.chars().take(60).collect();
                format!(
                    "Delegating to the `{}` subagent: {short}…",
                    subagent_type.as_deref().unwrap_or("explore")
                )
            }
            ToolCall::ListMcpServers { .. } => "Listing MCP servers…".into(),
            ToolCall::AddMcpServer { name, .. } => {
                format!("Adding MCP server `{name}`…")
            }
            ToolCall::RemoveMcpServer { name } => {
                format!("Removing MCP server `{name}`…")
            }
            ToolCall::AttachFile { path } => {
                format!("Indexing `{}`…", display_name(path))
            }
            ToolCall::SearchAttachedFiles { query, .. } => {
                format!("Searching attachments for \"{query}\"…")
            }
            ToolCall::DetachFile { path } => {
                format!("Detaching `{}`…", display_name(path))
            }
            ToolCall::TranscribeAudio { path, .. } => {
                format!("Transcribing `{}`…", display_name(path))
            }
            ToolCall::TreeSitterQuery { path, query, .. } => {
                let short: String = query.chars().take(60).collect();
                format!("Tree-sitter query on `{}`: `{short}`…", display_name(path))
            }
            ToolCall::AnalyzeBug { .. } => "Analyzing the reported bug…".into(),
            ToolCall::ReviewCode { .. } => "Reviewing code…".into(),
            ToolCall::ViewRepoMap { top_n, .. } => {
                format!("Building repo map (top {} symbols)…", top_n.unwrap_or(60))
            }
            ToolCall::BrowseWeb { url, action } => match action.as_deref() {
                Some("screenshot") => format!("Taking a screenshot of `{url}`…"),
                _ => format!("Fetching `{url}`…"),
            },
        }
    }
}

fn display_name(path: &str) -> String {
    path.rsplit(['/', '\\']).next().unwrap_or(path).to_string()
}

/// Structured result of one tool execution (camelCase over the wire).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolResult {
    pub success: bool,
    pub tool: String,
    pub summary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stdout: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stats: Option<serde_json::Value>,
    pub duration_ms: u64,
}

impl ToolResult {
    pub fn ok(
        tool: &str,
        summary: String,
        stdout: Option<String>,
        stats: Option<serde_json::Value>,
    ) -> Self {
        Self {
            success: true,
            tool: tool.to_string(),
            summary,
            stdout,
            error: None,
            stats,
            duration_ms: 0,
        }
    }

    pub fn err(tool: &str, summary: String, error: String) -> Self {
        Self {
            success: false,
            tool: tool.to_string(),
            summary,
            stdout: None,
            error: Some(error),
            stats: None,
            duration_ms: 0,
        }
    }
}

/// Real-time event emitted to the frontend for the agent timeline.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentToolEvent {
    pub id: String,
    pub tool: String,
    pub status: String, // "running" | "done" | "error"
    pub summary: String,
    pub started_at: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// Process exit code for shell tools, when reported.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Session this call belongs to, so the UI can attach it to the right
    /// turn even when events land after `activeSessionId` changes.
    pub session_id: u64,
}

/// Real-time event emitted when a first-class subagent run starts/completes.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SubagentRunEvent {
    pub parent_session_id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub child_session_id: Option<u64>,
    pub profile: String,
    pub status: String, // "started" | "completed" | "error"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    pub started_at: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub leased_workers: Option<usize>,
}

/// Event asking the user to approve a policy-`ask` tool call.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionRequestEvent {
    pub request_id: String,
    pub tool: String,
    pub summary: String,
    pub timestamp_ms: u64,
    /// Independent LLM review of a shell command (Bionic §3.3 hardening):
    /// a one-line second opinion rendered alongside the approval buttons.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub review: Option<String>,
}

/// Event asking the user a blocking question on behalf of the agent
/// (`ask_question` tool, P1-9). Emitted on `agent://question-request`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QuestionRequestEvent {
    pub request_id: String,
    pub question: String,
    /// Preset answer buttons; may be empty (free-text only).
    pub choices: Vec<String>,
    pub timestamp_ms: u64,
}

/// Event emitted on `agent://hitl-gate` when the model emits a `<hitl_gate>`
/// marker ahead of its tool-call batch. The harness blocks the loop until the
/// user answers via `agent_respond_hitl` (or a 10-minute timeout fires).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HitlGateEvent {
    pub request_id: String,
    pub session_id: u64,
    pub reason: String,
    /// Human-readable summaries of the tool calls waiting on this decision.
    pub tools: Vec<String>,
    pub timestamp_ms: u64,
}

/// The user's verdict on a pending `<hitl_gate>` request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct HitlDecision {
    pub approve: bool,
}

/// Event telling the frontend a file was changed by the agent (for editor sync
/// and diff preview).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileChangedEvent {
    pub path: String,
    /// "diff" for apply_file_diff, "write" for write_file.
    pub kind: String,
    /// Unified diff of the change, when computable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diff: Option<String>,
    /// Full pre-change file content (for undo/revert).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub before: Option<String>,
}

/// Lifecycle of one proposed file edit as seen by the UI badges.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EditStatus {
    Pending,
    Applied,
    Rejected,
    Failed,
    Conflict,
}

/// One proposed file change waiting on (or resolved by) the user review.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileEdit {
    pub id: String,
    /// Workspace-relative path (forward slashes) when inside the workspace,
    /// otherwise the absolute path (already vetted by the policy layer).
    pub path: String,
    /// 1-based inclusive line range the edit targets, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line_start: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line_end: Option<u64>,
    /// "patch" | "search_replace" | "full_content".
    pub kind: String,
    /// Unified diff of before -> after, for the review pane.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diff: Option<String>,
    /// Full pre-change content (review pane + undo handling).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub before: Option<String>,
    /// Full post-change content, computed but not yet written.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after: Option<String>,
    /// Free-form reason/summary supplied by the agent.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub reason: Option<String>,
    pub status: EditStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Bundle of edits from one tool call, keyed by group id. Carried over
/// `agent://edit-proposed` / `agent://edit-applied` / `agent://edit-failed`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EditBatchEvent {
    pub session_id: u64,
    pub group_id: String,
    pub edits: Vec<FileEdit>,
    pub timestamp_ms: u64,
}

/// Internal verdict the user (via `apply_edits` / `reject_edits`) sends back to
/// the tool call that is blocked awaiting review.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditDecision {
    Approve,
    Reject,
}

/// A proposed-edit batch held while the user reviews it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingEditGroup {
    pub id: String,
    pub session_id: u64,
    pub tool: String,
    pub edits: Vec<FileEdit>,
}

/// Pre-apply snapshot of one edited file (stored under `{workspace}/.ai/undo/`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileCheckpoint {
    pub id: String,
    pub path: String,
    pub timestamp_ms: u64,
    pub edit_id: String,
    /// sha256 of the snapshot content, for conflict checks.
    pub content_hash: String,
}

/// Per-plan-item progress event (blueprint §11 `step_started`/`step_completed`).
/// Emitted on `agent://plan-step`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanStepEvent {
    pub session_id: u64,
    pub plan_id: String,
    /// 1-based index of the plan item.
    pub item_index: usize,
    pub title: String,
    /// "in_progress" | "completed" | "terminal" | "failed".
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Result of the post-edit background verify pass. Emitted on
/// `agent://verify-result` so the UI can toast + log it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifyResultEvent {
    pub session_id: u64,
    pub ok: bool,
    /// Program/shell command that ran (empty when only LSP diagnostics ran).
    pub command: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    /// Truncated combined output for display.
    pub output: String,
    pub timestamp_ms: u64,
}

/// Live todo-list snapshot emitted on `agent://todo-update` after every
/// todo-tool mutation (Bionic §3.2 PLANNING).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TodoUpdateEvent {
    pub items: Vec<todo::TodoItem>,
    pub updated_at: u64,
}

/// Emitted when context trimming evicts >50% of non-pinned messages.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextTrimmedEvent {
    pub session_id: u64,
    pub dropped: usize,
    pub remaining: usize,
}

/// Per-step run trace emitted on `agent://run-trace` after each generation
/// step, so the UI can render a tool/token/duration timeline per turn.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunTraceEvent {
    pub session_id: u64,
    /// 1-based step number, matching `agent-step`.
    pub step_index: usize,
    /// "Plan" / "Execute" / "Subtask i/n · …" / "Subagent · …" / "Plan item i/n · …".
    pub group: String,
    /// Tool names emitted by that step (empty when it produced no tool calls).
    pub tools: Vec<String>,
    /// Wall-clock duration of the step (generation + tool execution).
    pub duration_ms: u64,
    /// Prompt tokens consumed by this step's generation.
    pub tokens_in: u64,
    /// Output tokens produced by this step's generation.
    pub tokens_out: u64,
    /// "completed" | "failed" | "cancelled".
    pub outcome: String,
    pub timestamp_ms: u64,
}

/// Bounded per-session run-trace store (P3.2). Keeps the most recent
/// [`MAX_TRACE_SESSIONS`] sessions, each with at most [`MAX_TRACE_EVENTS`]
/// steps, so the HTTP trace endpoint (`GET /v1/agent/sessions/{id}/trace`) and
/// the UI can export a run without unbounded memory retention.
pub const MAX_TRACE_SESSIONS: usize = 50;
pub const MAX_TRACE_EVENTS: usize = 500;

#[derive(Default)]
pub struct TraceStore {
    sessions: std::collections::VecDeque<(u64, Vec<RunTraceEvent>)>,
}

impl TraceStore {
    pub fn record(&mut self, event: RunTraceEvent) {
        let sid = event.session_id;
        if let Some(entry) = self.sessions.iter_mut().find(|(id, _)| *id == sid) {
            entry.1.push(event);
            if entry.1.len() > MAX_TRACE_EVENTS {
                let excess = entry.1.len() - MAX_TRACE_EVENTS;
                entry.1.drain(0..excess);
            }
        } else {
            self.sessions.push_back((sid, vec![event]));
            while self.sessions.len() > MAX_TRACE_SESSIONS {
                self.sessions.pop_front();
            }
        }
    }

    pub fn get(&self, session_id: u64) -> Vec<RunTraceEvent> {
        self.sessions
            .iter()
            .find(|(id, _)| *id == session_id)
            .map(|(_, v)| v.clone())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod trace_store_tests {
    use super::{RunTraceEvent, TraceStore, MAX_TRACE_EVENTS, MAX_TRACE_SESSIONS};

    fn ev(session: u64, step: usize) -> RunTraceEvent {
        RunTraceEvent {
            session_id: session,
            step_index: step,
            group: "Execute".into(),
            tools: Vec::new(),
            duration_ms: 1,
            tokens_in: 0,
            tokens_out: 0,
            outcome: "completed".into(),
            timestamp_ms: 0,
        }
    }

    #[test]
    fn groups_by_session() {
        let mut store = TraceStore::default();
        store.record(ev(1, 1));
        store.record(ev(1, 2));
        store.record(ev(2, 1));
        assert_eq!(store.get(1).len(), 2);
        assert_eq!(store.get(2).len(), 1);
        assert!(store.get(99).is_empty());
    }

    #[test]
    fn evicts_oldest_session_beyond_cap() {
        let mut store = TraceStore::default();
        for s in 0..(MAX_TRACE_SESSIONS as u64 + 5) {
            store.record(ev(s, 1));
        }
        assert!(store.get(0).is_empty(), "oldest session evicted");
        let newest = MAX_TRACE_SESSIONS as u64 + 4;
        assert_eq!(store.get(newest).len(), 1);
    }

    #[test]
    fn per_session_events_trim_to_cap() {
        let mut store = TraceStore::default();
        for i in 0..(MAX_TRACE_EVENTS + 3) {
            store.record(ev(7, i));
        }
        let got = store.get(7);
        assert_eq!(got.len(), MAX_TRACE_EVENTS);
        assert_eq!(got.last().unwrap().step_index, MAX_TRACE_EVENTS + 2);
    }
}

/// A user message submitted while the agent was already running, waiting to be
/// folded into the conversation at the next step boundary.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SteerMessage {
    /// Correlates with the `agent://steer-applied` event.
    pub id: String,
    pub session_id: u64,
    pub text: String,
}

/// Emitted once a queued steer has been folded into the running conversation,
/// so the UI can move it out of the queue and show it as a real user turn.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SteerAppliedEvent {
    pub id: String,
    pub session_id: u64,
    pub text: String,
    /// Agent step the steer landed on, for ordering in the timeline.
    pub step: usize,
}

/// How a human answered a policy-`ask` permission request. Carried over the
/// permission response channel and serialized to the UI as the button choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionDecision {
    /// Allow exactly this one call; ask again next time.
    AllowOnce,
    /// Allow every call of this tool (exact command, for terminal) for the rest
    /// of the session.
    AllowSession,
    /// Allow forever by writing an `allow` rule to `.ai/policy.json`.
    AlwaysAllow,
    /// Deny this call.
    Deny,
}

/// Per-session grant for a path OUTSIDE the workspace (Bionic §3.3
/// `{path, mode}`). Never persisted — grants die with the app session.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PathGrant {
    pub path: PathBuf,
    /// "read" — read-only tools may touch this path;
    /// "write" — mutating file tools may touch it too.
    pub mode: String,
}

/// Short-lived event emitted to the frontend for background task lifecycle.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackgroundTaskEvent {
    pub task_id: String,
    pub session_id: u64,
    pub label: String,
    /// "started" | "completed" | "error" | "aborted".
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// Background task registry entry.
pub struct BackgroundTaskEntry {
    pub info: BackgroundTaskInfo,
    pub cancel: tokio_util::sync::CancellationToken,
}

/// Serialisable info about a background task, returned to the frontend.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackgroundTaskInfo {
    pub id: String,
    pub session_id: u64,
    pub label: String,
    /// "running" | "completed" | "error" | "aborted".
    pub status: String,
    pub started_at: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
}

/// Thread-safe registry for background tasks with per-task cancellation.
pub struct BackgroundRegistry {
    tasks: std::sync::Mutex<HashMap<String, BackgroundTaskEntry>>,
    next_id: AtomicU64,
}

impl Default for BackgroundRegistry {
    fn default() -> Self {
        Self {
            tasks: std::sync::Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
        }
    }
}

impl BackgroundRegistry {
    /// Register a new background task; returns `(task_id, cancel_token)`.
    pub fn register(&self, session_id: u64, label: String) -> (String, CancellationToken) {
        let id = format!("bg-{}", self.next_id.fetch_add(1, Ordering::SeqCst));
        let cancel = CancellationToken::new();
        let entry = BackgroundTaskEntry {
            info: BackgroundTaskInfo {
                id: id.clone(),
                session_id,
                label,
                status: "running".into(),
                started_at: now_ms(),
                duration_ms: None,
            },
            cancel: cancel.clone(),
        };
        self.tasks.lock().unwrap().insert(id.clone(), entry);
        (id, cancel)
    }

    /// Mark a task as completed and remove it after a short delay.
    pub fn finish(&self, task_id: &str, status: &str, _detail: Option<String>) {
        let mut tasks = self.tasks.lock().unwrap();
        if let Some(entry) = tasks.get_mut(task_id) {
            entry.info.status = status.to_string();
            entry.info.duration_ms = Some(now_ms().saturating_sub(entry.info.started_at));
        }
        // Remove immediately — the frontend will receive the event and clean up.
        tasks.remove(task_id);
    }

    /// Abort a task by id (cancels its token).
    pub fn abort(&self, task_id: &str) -> Option<BackgroundTaskInfo> {
        let mut tasks = self.tasks.lock().unwrap();
        if let Some(entry) = tasks.get(task_id) {
            entry.cancel.cancel();
            let mut info = entry.info.clone();
            info.status = "aborted".into();
            info.duration_ms = Some(now_ms().saturating_sub(info.started_at));
            tasks.remove(task_id);
            return Some(info);
        }
        None
    }

    /// List all currently running background tasks.
    pub fn list(&self) -> Vec<BackgroundTaskInfo> {
        self.tasks
            .lock()
            .unwrap()
            .values()
            .map(|e| {
                let mut info = e.info.clone();
                if info.status == "running" {
                    info.duration_ms = Some(now_ms().saturating_sub(info.started_at));
                }
                info
            })
            .collect()
    }

    /// Count running tasks.
    #[allow(dead_code)]
    pub fn active_count(&self) -> usize {
        self.tasks.lock().unwrap().len()
    }
}

/// How many background tasks keep an output buffer (oldest pruned first).
const BG_OUTPUT_MAX_TASKS: usize = 16;
/// Max retained lines per background task buffer.
const BG_OUTPUT_MAX_LINES: usize = 400;
/// Max characters retained for one not-yet-complete line of streamed tokens.
const BG_OUTPUT_MAX_PARTIAL: usize = 8_192;
/// Max characters of any single stored line.
const BG_OUTPUT_LINE_CAP: usize = 2_000;

struct TaskOutputBuffer {
    lines: VecDeque<String>,
    /// Tokens accumulated since the last newline (streamed assistant text).
    partial: String,
}

#[derive(Default)]
struct BackgroundOutputInner {
    /// session_id → buffer. Survives task completion so the drawer can show
    /// post-mortem output after the registry entry is removed.
    buffers: HashMap<u64, TaskOutputBuffer>,
    /// task_id → session_id (resolved once at bind time).
    tasks: HashMap<String, u64>,
    /// Insertion order of session ids, for pruning the oldest task.
    order: Vec<u64>,
}

/// Ring buffer of recent output per background task. Background tasks run the
/// agent loop (not a shell), so "output" is the streamed assistant text plus
/// tool/step/done lines captured from the shared event funnel.
pub struct BackgroundOutputStore {
    inner: std::sync::Mutex<BackgroundOutputInner>,
}

impl Default for BackgroundOutputStore {
    fn default() -> Self {
        Self {
            inner: std::sync::Mutex::new(BackgroundOutputInner::default()),
        }
    }
}

impl BackgroundOutputStore {
    /// Bind a task id to its session, starting (or reusing) an output buffer.
    pub fn bind(&self, task_id: &str, session_id: u64) {
        let mut inner = self.inner.lock().unwrap();
        if !inner.tasks.contains_key(task_id) {
            inner.tasks.insert(task_id.to_string(), session_id);
        }
        if !inner.buffers.contains_key(&session_id) {
            inner.order.push(session_id);
        }
        inner
            .buffers
            .entry(session_id)
            .or_insert_with(|| TaskOutputBuffer {
                lines: VecDeque::new(),
                partial: String::new(),
            });
        while inner.order.len() > BG_OUTPUT_MAX_TASKS {
            let Some(old) = inner.order.first().copied() else {
                break;
            };
            inner.order.remove(0);
            inner.buffers.remove(&old);
            inner.tasks.retain(|_, sid| *sid != old);
        }
    }

    /// Append a complete line (tool/step/done/error), flushing any partial
    /// assistant text first so ordering is preserved.
    pub fn record(&self, session_id: u64, line: &str) {
        let mut inner = self.inner.lock().unwrap();
        let Some(buf) = inner.buffers.get_mut(&session_id) else {
            return;
        };
        Self::flush_partial(buf);
        Self::push_line(buf, line);
    }

    /// Append a streamed token delta; complete lines are drained as they form.
    pub fn record_token(&self, session_id: u64, delta: &str) {
        let mut inner = self.inner.lock().unwrap();
        let Some(buf) = inner.buffers.get_mut(&session_id) else {
            return;
        };
        buf.partial.push_str(delta);
        while let Some(pos) = buf.partial.find('\n') {
            let line: String = buf.partial.drain(..=pos).collect();
            Self::push_line(buf, line.trim_end_matches('\n'));
        }
        if buf.partial.len() > BG_OUTPUT_MAX_PARTIAL {
            let overflow = std::mem::take(&mut buf.partial);
            Self::push_line(buf, &overflow);
        }
    }

    /// Push any remaining partial text as a final line (call on done/error).
    pub fn flush(&self, session_id: u64) {
        let mut inner = self.inner.lock().unwrap();
        let Some(buf) = inner.buffers.get_mut(&session_id) else {
            return;
        };
        Self::flush_partial(buf);
    }

    /// Last `tail_lines` (default 200) for a task id; empty when unknown.
    pub fn tail(&self, task_id: &str, tail_lines: Option<usize>) -> Vec<String> {
        let inner = self.inner.lock().unwrap();
        let Some(sid) = inner.tasks.get(task_id) else {
            return Vec::new();
        };
        let Some(buf) = inner.buffers.get(sid) else {
            return Vec::new();
        };
        let n = tail_lines.unwrap_or(200).clamp(1, BG_OUTPUT_MAX_LINES);
        buf.lines.iter().rev().take(n).rev().cloned().collect()
    }

    fn flush_partial(buf: &mut TaskOutputBuffer) {
        if !buf.partial.is_empty() {
            let partial = std::mem::take(&mut buf.partial);
            Self::push_line(buf, &partial);
        }
    }

    fn push_line(buf: &mut TaskOutputBuffer, line: &str) {
        if line.is_empty() {
            return;
        }
        let clipped: String = line.chars().take(BG_OUTPUT_LINE_CAP).collect();
        buf.lines.push_back(clipped);
        while buf.lines.len() > BG_OUTPUT_MAX_LINES {
            buf.lines.pop_front();
        }
    }
}

/// Long-lived agent state managed by Tauri.
pub struct ToolState {
    /// Current workspace root, shared with the workspace picker.
    pub workspace: Mutex<Vec<PathBuf>>,
    /// YOLO sub-mode (Bionic §3.3): auto-approve ROUTINE shell commands
    /// (never red-zone). Toggled from the UI; session-only.
    pub yolo: std::sync::atomic::AtomicBool,
    /// Per-session path grants for paths outside the workspace.
    pub path_grants: std::sync::Mutex<Vec<PathGrant>>,
    /// Cached MCP server connections, keyed by `McpServerConfig::cache_key()`.
    pub mcp_servers: Mutex<HashMap<String, std::sync::Arc<tokio::sync::Mutex<mcp::McpClient>>>>,
    /// Monotonic event id counter.
    pub id: AtomicU64,
    /// Pending permission requests keyed by request id (see `policy::check`).
    pub permission_requests:
        Mutex<HashMap<String, tokio::sync::oneshot::Sender<PermissionDecision>>>,
    /// Pending `ask_question` requests keyed by request id (P1-9). The agent
    /// blocks on the channel until the user answers via `agent_respond_question`.
    pub pending_questions: Mutex<HashMap<String, tokio::sync::oneshot::Sender<String>>>,
    /// Pending `<hitl_gate>` approval requests keyed by request id. The agent
    /// loop blocks on the channel until `agent_respond_hitl` arrives.
    pub pending_hitl: Mutex<HashMap<String, tokio::sync::oneshot::Sender<HitlDecision>>>,
    /// Monotonic permission request id counter.
    pub request_id: AtomicU64,
    /// Tools the user "allowed for this session" (see `PermissionDecision`).
    /// Keyed by tool name, or `execute_terminal_command:<command>` for terminal
    /// calls so one approved command never silently covers a different one.
    pub session_allow: std::sync::Mutex<HashSet<String>>,
    /// Skills & rules snapshot, shared with the `KnowledgeState` managed state
    /// so the `read_skill` tool can load any skill's full text on demand.
    pub knowledge: std::sync::Arc<skills::KnowledgeState>,
    /// RAG attachment index (chunked + embedded attached files).
    pub rag: std::sync::Mutex<rag::AttachmentIndex>,
    /// Active persisted plan for the workspace (`.ai/plan.json`), if any.
    pub plan: std::sync::Mutex<Option<plan::PlanState>>,
    /// Guards against nested `execute_plan` re-entry (a plan executing itself).
    pub plan_executing: std::sync::Mutex<bool>,
    /// Clone of the engine pool so the `execute_plan` tool can drive its own
    /// per-item agent loops. Kept in sync with `InferenceState.pool` on every
    /// load/unload/configure.
    pub engine: tokio::sync::Mutex<Option<std::sync::Arc<crate::engine::EnginePool>>>,
    /// Clone of the emitter channel so nested plan-item loops can stream
    /// tokens/steps to the UI. Kept in sync with `InferenceState.worker_tx`.
    #[allow(dead_code)]
    pub worker_tx: std::sync::Mutex<Option<crossbeam_channel::Sender<crate::engine::WorkerEvent>>>,
    /// The session currently running the agent loop (for plan-step events).
    pub session_id: std::sync::atomic::AtomicU64,
    /// Engine worker indexes currently leased to running subagents (P1-8
    /// occupancy leasing). Index 0 is reserved for the primary agent loop;
    /// children lease from 1..pool.len(). Keyed per pool generation — entries
    /// are dropped as soon as the lease guard falls out of scope.
    pub leased_workers: std::sync::Mutex<HashSet<usize>>,
    /// Background task registry (P2-12): tracks tasks running independently
    /// of the foreground chat, each with its own cancellation token.
    pub background_tasks: BackgroundRegistry,
    /// Per-task output ring buffers for background tasks (P2.2): streamed
    /// assistant text + tool/step lines, keyed by session id.
    pub background_output: BackgroundOutputStore,
    /// Cached TF-IDF index for semantic search (P0-3). Invalidated when
    /// workspace changes or a different root/include is requested.
    pub sem_index: std::sync::Mutex<Option<tools::SemIndex>>,
    /// Cached symbol graph for the repo map, keyed by file -> (mtime, subgraph).
    /// Keyed by workspace root to avoid cross-workspace collisions.
    pub repo_graph: tokio::sync::Mutex<HashMap<String, (u64, tools::RepoGraph)>>,
    /// Tracks whether an auto-checkpoint has been created for the current
    /// agent step. Set to `false` at the start of each step; the first
    /// file-editing tool call auto-checkpoints and sets this to `true`.
    pub step_checkpointed: std::sync::atomic::AtomicBool,
    /// User messages submitted mid-run, waiting to be folded into the
    /// conversation at the top of the next agent step.
    ///
    /// The step loop's `messages` vec is private to the running thread, so a
    /// message pushed through `ContextState` would never be seen by a run that
    /// is already in flight. Queueing here and draining inside the loop is what
    /// makes "steer" take effect on a turn that is already running.
    pub steer_queue: std::sync::Mutex<VecDeque<SteerMessage>>,
    /// Proposed file edits awaiting user review, keyed by group id.
    pub pending_edits: std::sync::Mutex<HashMap<String, PendingEditGroup>>,
    /// Decision channels for blocked edit tools, keyed by group id. The tool
    /// awaits the oneshot until the user answers via `apply_edits` /
    /// `reject_edits` (or a 10-minute timeout fires).
    pub edit_decisions: Mutex<HashMap<String, tokio::sync::oneshot::Sender<EditDecision>>>,
    /// Pre-apply file snapshots for `checkpoint_restore`, newest last.
    pub file_checkpoints: std::sync::Mutex<Vec<FileCheckpoint>>,
    /// LSP diagnostics manager, shared with the editor gutter (`lsp_diagnostics`
    /// command). The verify pass uses it to fold real diagnostics into the
    /// post-edit report shown to the model.
    pub lsp: std::sync::Arc<crate::lsp::LspManager>,
    /// Bounded per-session run traces (P3.2), feeding the HTTP trace endpoint
    /// and the UI export.
    pub traces: std::sync::Mutex<TraceStore>,
}

impl Default for ToolState {
    fn default() -> Self {
        Self {
            workspace: Mutex::new(Vec::new()),
            yolo: std::sync::atomic::AtomicBool::new(false),
            path_grants: std::sync::Mutex::new(Vec::new()),
            mcp_servers: Mutex::new(HashMap::new()),
            id: AtomicU64::new(1),
            permission_requests: Mutex::new(HashMap::new()),
            pending_questions: Mutex::new(HashMap::new()),
            pending_hitl: Mutex::new(HashMap::new()),
            request_id: AtomicU64::new(1),
            session_allow: std::sync::Mutex::new(HashSet::new()),
            knowledge: std::sync::Arc::new(skills::KnowledgeState::default()),
            rag: std::sync::Mutex::new(rag::AttachmentIndex::default()),
            plan: std::sync::Mutex::new(None),
            plan_executing: std::sync::Mutex::new(false),
            engine: tokio::sync::Mutex::new(None),
            worker_tx: std::sync::Mutex::new(None),
            session_id: std::sync::atomic::AtomicU64::new(0),
            leased_workers: std::sync::Mutex::new(HashSet::new()),
            background_tasks: BackgroundRegistry::default(),
            background_output: BackgroundOutputStore::default(),
            sem_index: std::sync::Mutex::new(None),
            repo_graph: tokio::sync::Mutex::new(HashMap::new()),
            step_checkpointed: std::sync::atomic::AtomicBool::new(false),
            steer_queue: std::sync::Mutex::new(VecDeque::new()),
            pending_edits: std::sync::Mutex::new(HashMap::new()),
            edit_decisions: Mutex::new(HashMap::new()),
            file_checkpoints: std::sync::Mutex::new(Vec::new()),
            lsp: std::sync::Arc::new(crate::lsp::LspManager::default()),
            traces: std::sync::Mutex::new(TraceStore::default()),
        }
    }
}

impl ToolState {
    pub fn next_event_id(&self) -> String {
        format!("tool-{}", self.id.fetch_add(1, Ordering::SeqCst))
    }

    /// Append a run-trace step to the bounded per-session store (P3.2).
    pub fn record_trace(&self, event: RunTraceEvent) {
        if let Ok(mut store) = self.traces.lock() {
            store.record(event);
        }
    }

    /// Snapshot the recorded run trace for a session (empty when unknown).
    pub fn traces_for(&self, session_id: u64) -> Vec<RunTraceEvent> {
        self.traces
            .lock()
            .map(|store| store.get(session_id))
            .unwrap_or_default()
    }

    /// Queue a user message for injection at the top of the next agent step.
    ///
    /// Returns the id assigned to it so the UI can correlate the eventual
    /// `agent://steer-applied` event with the message it sent.
    pub fn enqueue_steer(&self, session_id: u64, text: String) -> String {
        let id = format!("steer-{}", self.id.fetch_add(1, Ordering::SeqCst));
        let msg = SteerMessage {
            id: id.clone(),
            session_id,
            text,
        };
        if let Ok(mut q) = self.steer_queue.lock() {
            q.push_back(msg);
        }
        id
    }

    /// Take every queued steer addressed to `session_id`, oldest first.
    ///
    /// Steers for other sessions are left in place: a stale message must not
    /// leak into the next chat's run. This drains at a step boundary, so a steer
    /// submitted mid-generation takes effect from the following step.
    pub fn take_steers(&self, session_id: u64) -> Vec<SteerMessage> {
        let Ok(mut q) = self.steer_queue.lock() else {
            return Vec::new();
        };
        let mut mine = Vec::new();
        let mut kept = VecDeque::new();
        while let Some(m) = q.pop_front() {
            if m.session_id == session_id {
                mine.push(m);
            } else {
                kept.push_back(m);
            }
        }
        *q = kept;
        mine
    }

    /// Drop queued steers, e.g. when a run ends or is aborted so they do not
    /// leak into the next turn.
    pub fn clear_steers(&self) {
        if let Ok(mut q) = self.steer_queue.lock() {
            q.clear();
        }
    }

    pub fn next_request_id(&self) -> String {
        format!("perm-{}", self.request_id.fetch_add(1, Ordering::SeqCst))
    }

    /// Fresh id for a pending `ask_question` request.
    pub fn next_question_id(&self) -> String {
        format!("q-{}", self.request_id.fetch_add(1, Ordering::SeqCst))
    }

    /// Fresh id for a pending `<hitl_gate>` approval request.
    pub fn next_hitl_id(&self) -> String {
        format!("hitl-{}", self.request_id.fetch_add(1, Ordering::SeqCst))
    }

    /// Fresh group id for a proposed-edit batch.
    pub fn next_edit_group_id(&self) -> String {
        format!("edit-{}", self.request_id.fetch_add(1, Ordering::SeqCst))
    }

    /// Fresh id for one proposed file edit within a batch.
    pub fn next_edit_id(&self) -> String {
        format!("ed-{}", self.id.fetch_add(1, Ordering::SeqCst))
    }

    /// Fresh id for a pre-apply file snapshot.
    pub fn next_checkpoint_id(&self) -> String {
        format!("snap-{}", self.request_id.fetch_add(1, Ordering::SeqCst))
    }

    /// Remember which session is running the agent loop, so the `execute_plan`
    /// tool can address its `plan-step` events to the correct chat message.
    pub fn note_session(&self, session_id: u64) {
        self.session_id.store(session_id, Ordering::SeqCst);
    }

    /// Return the first (primary) workspace root, or `None` if no workspace set.
    pub async fn primary_workspace(&self) -> Option<PathBuf> {
        self.workspace.lock().await.first().cloned()
    }

    /// Return all workspace roots.
    #[allow(dead_code)]
    pub async fn all_workspaces(&self) -> Vec<PathBuf> {
        self.workspace.lock().await.clone()
    }

    /// Path of the session-permissions persistence file for a workspace.
    fn permissions_file(workspace: &Path) -> PathBuf {
        workspace.join(".ai").join("session-permissions.json")
    }

    /// Persist the current in-memory session permissions to
    /// `{workspace}/.ai/session-permissions.json` (first workspace root).
    pub fn save_session_allow(&self) {
        let Some(path) = self.permissions_path() else {
            return;
        };
        if let Some(parent) = path.parent() {
            if std::fs::create_dir_all(parent).is_err() {
                crate::logging::warn(
                    None,
                    "policy.permissions",
                    &format!("could not create {}", parent.display()),
                );
                return;
            }
        }
        let entries: Vec<String> = {
            let allow = self.session_allow.lock().unwrap();
            let mut v: Vec<String> = allow.iter().cloned().collect();
            v.sort();
            v
        };
        let payload = serde_json::json!({ "tools": entries });
        if std::fs::write(
            &path,
            serde_json::to_string_pretty(&payload).unwrap_or_default(),
        )
        .is_err()
        {
            crate::logging::warn(
                None,
                "policy.permissions",
                &format!(
                    "failed to persist session permissions to {}",
                    path.display()
                ),
            );
        }
    }

    /// Load previously persisted session permissions from
    /// `{workspace}/.ai/session-permissions.json` into `session_allow`.
    /// Silently no-ops when the file is absent or malformed — the user simply
    /// re-approves once and the next save rewrites it.
    pub fn load_session_allow(&self, workspace: &Path) {
        let path = Self::permissions_file(workspace);
        let Ok(bytes) = std::fs::read(&path) else {
            return;
        };
        let Ok(payload) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
            return;
        };
        let Some(tools) = payload.get("tools").and_then(|t| t.as_array()) else {
            return;
        };
        let mut allow = self.session_allow.lock().unwrap();
        for t in tools {
            if let Some(s) = t.as_str() {
                if !s.trim().is_empty() {
                    allow.insert(s.to_string());
                }
            }
        }
    }

    /// Resolve the persistence file path from the primary workspace root.
    fn permissions_path(&self) -> Option<PathBuf> {
        // Primary workspace is a tokio mutex — we cannot block here on a std
        // mutex; take the first entry via try_lock and fall back to scraping
        // the permissions for the saved path when the lock is contended.
        match self.workspace.try_lock() {
            Ok(guard) => guard.first().cloned().map(|ws| Self::permissions_file(&ws)),
            Err(_) => None,
        }
    }
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Root of all per-session scratchpad folders, deliberately OUTSIDE any
/// workspace (Bionic §3.2 `get_scratchpad_folder`). File tools may read/write
/// here without a workspace grant; everything else outside the workspace stays
/// default-deny (see `policy::check`).
pub fn scratchpad_root() -> PathBuf {
    std::env::temp_dir().join("ai-editor-scratchpad")
}

/// Absolute scratchpad folder for one agent session (`session-<id>`), created
/// on demand.
pub fn session_scratchpad(session_id: u64) -> PathBuf {
    scratchpad_root().join(format!("session-{session_id}"))
}

#[cfg(test)]
mod steer_tests {
    use super::*;

    #[test]
    fn enqueued_steer_is_returned_in_order() {
        let s = ToolState::default();
        let a = s.enqueue_steer(7, "first".into());
        let b = s.enqueue_steer(7, "second".into());

        let got = s.take_steers(7);
        assert_eq!(got.len(), 2, "both steers should be delivered");
        assert_eq!(got[0].text, "first");
        assert_eq!(got[1].text, "second");
        assert_eq!(got[0].id, a);
        assert_eq!(got[1].id, b);
    }

    #[test]
    fn take_steers_drains_so_a_second_take_is_empty() {
        let s = ToolState::default();
        s.enqueue_steer(1, "once".into());
        assert_eq!(s.take_steers(1).len(), 1);
        assert!(
            s.take_steers(1).is_empty(),
            "a steer must not be applied twice"
        );
    }

    #[test]
    fn steers_do_not_leak_across_sessions() {
        let s = ToolState::default();
        s.enqueue_steer(1, "for session one".into());
        s.enqueue_steer(2, "for session two".into());

        // Session 2 drains only its own; session 1's message survives so a
        // still-running session 1 does not lose it.
        let two = s.take_steers(2);
        assert_eq!(two.len(), 1);
        assert_eq!(two[0].text, "for session two");

        let one = s.take_steers(1);
        assert_eq!(one.len(), 1, "session 1's steer must still be queued");
        assert_eq!(one[0].text, "for session one");
    }

    #[test]
    fn clear_steers_drops_everything() {
        let s = ToolState::default();
        s.enqueue_steer(1, "a".into());
        s.enqueue_steer(1, "b".into());
        s.clear_steers();
        assert!(
            s.take_steers(1).is_empty(),
            "a finished run must not leak steers into the next one"
        );
    }

    #[test]
    fn ids_are_unique_per_steer() {
        let s = ToolState::default();
        let a = s.enqueue_steer(1, "x".into());
        let b = s.enqueue_steer(1, "y".into());
        assert_ne!(a, b, "ids must be distinct so the UI can match them up");
    }
}

#[cfg(test)]
mod background_output_tests {
    use super::*;

    #[test]
    fn unbound_sessions_are_ignored() {
        let s = BackgroundOutputStore::default();
        s.record(99, "nope");
        s.record_token(99, "nope");
        assert!(s.tail("bg-1", None).is_empty());
    }

    #[test]
    fn tail_returns_lines_in_order() {
        let s = BackgroundOutputStore::default();
        s.bind("bg-1", 1);
        s.record(1, "first");
        s.record(1, "second");
        assert_eq!(s.tail("bg-1", None), vec!["first", "second"]);
    }

    #[test]
    fn tokens_are_joined_until_a_newline() {
        let s = BackgroundOutputStore::default();
        s.bind("bg-1", 1);
        s.record_token(1, "hello ");
        s.record_token(1, "world\nnext");
        assert_eq!(s.tail("bg-1", None), vec!["hello world"]);
        s.flush(1);
        assert_eq!(s.tail("bg-1", None), vec!["hello world", "next"]);
    }

    #[test]
    fn tail_respects_tail_lines() {
        let s = BackgroundOutputStore::default();
        s.bind("bg-1", 1);
        for i in 0..10 {
            s.record(1, &format!("line {i}"));
        }
        let tail = s.tail("bg-1", Some(3));
        assert_eq!(tail, vec!["line 7", "line 8", "line 9"]);
    }

    #[test]
    fn buffer_survives_task_finish() {
        let s = BackgroundOutputStore::default();
        s.bind("bg-1", 1);
        s.record(1, "kept");
        s.flush(1);
        assert_eq!(s.tail("bg-1", None), vec!["kept"]);
    }

    #[test]
    fn oldest_tasks_are_pruned() {
        let s = BackgroundOutputStore::default();
        for i in 0..=(BG_OUTPUT_MAX_TASKS as u64) {
            s.bind(&format!("bg-{i}"), i);
            s.record(i, "line");
        }
        assert!(s.tail("bg-0", None).is_empty(), "oldest task pruned");
        assert_eq!(s.tail("bg-1", None), vec!["line"]);
    }

    #[test]
    fn lines_are_capped_in_length_and_count() {
        let s = BackgroundOutputStore::default();
        s.bind("bg-1", 1);
        let huge = "x".repeat(BG_OUTPUT_LINE_CAP * 2);
        s.record(1, &huge);
        let tail = s.tail("bg-1", None);
        assert_eq!(tail.len(), 1);
        assert_eq!(tail[0].chars().count(), BG_OUTPUT_LINE_CAP);

        for i in 0..(BG_OUTPUT_MAX_LINES + 50) {
            s.record(1, &format!("l{i}"));
        }
        assert_eq!(
            s.tail("bg-1", Some(BG_OUTPUT_MAX_LINES)).len(),
            BG_OUTPUT_MAX_LINES
        );
    }
}
