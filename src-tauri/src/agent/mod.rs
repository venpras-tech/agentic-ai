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

pub mod context;
pub mod core;
pub mod interrupt;
pub mod mcp;
pub mod orchestrator;
pub mod plan;
pub mod policy;
pub mod rag;
pub mod skills;
pub mod todo;
pub mod tools;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

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
            ToolCall::ListMcpServers { .. } => "list_mcp_servers",
            ToolCall::AddMcpServer { .. } => "add_mcp_server",
            ToolCall::RemoveMcpServer { .. } => "remove_mcp_server",
            ToolCall::AttachFile { .. } => "attach_file",
            ToolCall::SearchAttachedFiles { .. } => "search_attached_files",
            ToolCall::DetachFile { .. } => "detach_file",
            ToolCall::TranscribeAudio { .. } => "transcribe_audio",
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
            ToolCall::RunTests { .. } => "Running the test suite…".into(),
            ToolCall::WriteFile { path, .. } => {
                format!("Writing `{}`…", display_name(path))
            }
            ToolCall::CreateSkill { name, .. } => format!("Learning skill `{name}`…"),
            ToolCall::ReadSkill { name, .. } => format!("Loading skill `{name}`…"),
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

/// Long-lived agent state managed by Tauri.
pub struct ToolState {
    /// Current workspace root, shared with the workspace picker.
    pub workspace: Mutex<Option<PathBuf>>,
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
}

impl Default for ToolState {
    fn default() -> Self {
        Self {
            workspace: Mutex::new(None),
            yolo: std::sync::atomic::AtomicBool::new(false),
            path_grants: std::sync::Mutex::new(Vec::new()),
            mcp_servers: Mutex::new(HashMap::new()),
            id: AtomicU64::new(1),
            permission_requests: Mutex::new(HashMap::new()),
            request_id: AtomicU64::new(1),
            session_allow: std::sync::Mutex::new(HashSet::new()),
            knowledge: std::sync::Arc::new(skills::KnowledgeState::default()),
            rag: std::sync::Mutex::new(rag::AttachmentIndex::default()),
            plan: std::sync::Mutex::new(None),
            plan_executing: std::sync::Mutex::new(false),
            engine: tokio::sync::Mutex::new(None),
            worker_tx: std::sync::Mutex::new(None),
            session_id: std::sync::atomic::AtomicU64::new(0),
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

    /// Remember which session is running the agent loop, so the `execute_plan`
    /// tool can address its `plan-step` events to the correct chat message.
    pub fn note_session(&self, session_id: u64) {
        self.session_id.store(session_id, Ordering::SeqCst);
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
