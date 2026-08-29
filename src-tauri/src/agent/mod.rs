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
pub mod interrupt;
pub mod mcp;
pub mod orchestrator;
pub mod plan;
pub mod policy;
pub mod rag;
pub mod registry;
pub mod skills;
pub mod subagent;
pub mod todo;
pub mod tools;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Session this call belongs to, so the UI can attach it to the right
    /// turn even when events land after `activeSessionId` changes.
    pub session_id: u64,
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

/// Long-lived agent state managed by Tauri.
pub struct ToolState {
    /// Current workspace root, shared with the workspace picker.
    pub workspace: Mutex<Vec<PathBuf>>,
    /// YOLO sub-mode (Bionic §3.3): auto-approve ROUTINE shell commands
    /// (never red-zone). Toggled from the UI; session-only.
    pub yolo: std::sync::atomic::AtomicBool,
    /// Per-session path grants for paths outside the workspace.
    pub path_grants: std::sync::Mutex<Vec<PathGrant>>,
    /// Cached MCP server connections, keyed by `bin + args`.
    pub mcp_servers: Mutex<HashMap<String, std::sync::Arc<tokio::sync::Mutex<mcp::McpHandle>>>>,
    /// Monotonic event id counter.
    pub id: AtomicU64,
    /// Pending permission requests keyed by request id (see `policy::check`).
    pub permission_requests:
        Mutex<HashMap<String, tokio::sync::oneshot::Sender<PermissionDecision>>>,
    /// Pending `ask_question` requests keyed by request id (P1-9). The agent
    /// blocks on the channel until the user answers via `agent_respond_question`.
    pub pending_questions: Mutex<HashMap<String, tokio::sync::oneshot::Sender<String>>>,
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
            sem_index: std::sync::Mutex::new(None),
            repo_graph: tokio::sync::Mutex::new(HashMap::new()),
            step_checkpointed: std::sync::atomic::AtomicBool::new(false),
        }
    }
}

impl ToolState {
    pub fn next_event_id(&self) -> String {
        format!("tool-{}", self.id.fetch_add(1, Ordering::SeqCst))
    }

    pub fn next_request_id(&self) -> String {
        format!("perm-{}", self.request_id.fetch_add(1, Ordering::SeqCst))
    }

    /// Fresh id for a pending `ask_question` request.
    pub fn next_question_id(&self) -> String {
        format!("q-{}", self.request_id.fetch_add(1, Ordering::SeqCst))
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
        if std::fs::write(&path, serde_json::to_string_pretty(&payload).unwrap_or_default())
            .is_err()
        {
            crate::logging::warn(
                None,
                "policy.permissions",
                &format!("failed to persist session permissions to {}", path.display()),
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

