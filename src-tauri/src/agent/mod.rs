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
pub mod policy;
pub mod skills;
pub mod tools;

use std::collections::HashMap;
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
    ApplyFileDiff {
        path: String,
        diff: String,
    },
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
        server_bin: String,
        #[serde(default)]
        server_args: Vec<String>,
        tool: String,
        #[serde(default)]
        arguments: serde_json::Value,
        #[serde(default)]
        timeout_secs: Option<u64>,
    },
    #[serde(rename_all = "camelCase")]
    GitStatus {},
    #[serde(rename_all = "camelCase")]
    GitDiff {
        path: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    GitCommit {
        message: String,
    },
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
    WriteFile {
        path: String,
        content: String,
    },
    #[serde(rename_all = "camelCase")]
    CreateSkill {
        name: String,
        #[serde(default)]
        description: Option<String>,
        content: String,
    },
    #[serde(rename_all = "camelCase")]
    ReadSkill {
        name: String,
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
        }
    }

    /// A short human-readable description used in the UI timeline header.
    pub fn summary(&self) -> String {
        match self {
            ToolCall::GlobSearchCodebase { pattern, .. } => {
                format!("Scanning workspace for `{pattern}`…")
            }
            ToolCall::SearchFileContents { pattern, include, .. } => match include {
                Some(inc) if !inc.is_empty() => {
                    format!("Searching `{inc}` for `{pattern}`…")
                }
                _ => format!("Searching workspace for `{pattern}`…"),
            },
            ToolCall::ViewFileStructure { path, .. } => {
                format!("Parsing AST of `{}`…", display_name(path))
            }
            ToolCall::ReadFileRange { path, start_line, end_line } => {
                format!("Reading `{}` lines {start_line}..={end_line}…", display_name(path))
            }
            ToolCall::ApplyFileDiff { path, .. } => {
                format!("Applying edit to `{}`…", display_name(path))
            }
            ToolCall::ExecuteTerminalCommand { command, .. } => {
                format!("Executing `{command}`…")
            }
            ToolCall::CallMcpTool { tool, .. } => format!("Calling MCP tool `{tool}`…"),
            ToolCall::GitStatus { .. } => "Reading git status…".into(),
            ToolCall::GitDiff { path: Some(path), .. } => {
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
    pub fn ok(tool: &str, summary: String, stdout: Option<String>, stats: Option<serde_json::Value>) -> Self {
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

/// Long-lived agent state managed by Tauri.
pub struct ToolState {
    /// Current workspace root, shared with the workspace picker.
    pub workspace: Mutex<Option<PathBuf>>,
    /// Cached MCP server connections, keyed by `bin + args`.
    pub mcp_servers: Mutex<HashMap<String, std::sync::Arc<tokio::sync::Mutex<mcp::McpHandle>>>>,
    /// Monotonic event id counter.
    pub id: AtomicU64,
    /// Pending permission requests keyed by request id (see `policy::check`).
    pub permission_requests:
        Mutex<HashMap<String, tokio::sync::oneshot::Sender<bool>>>,
    /// Monotonic permission request id counter.
    pub request_id: AtomicU64,
    /// Skills & rules snapshot, shared with the `KnowledgeState` managed state
    /// so the `read_skill` tool can load any skill's full text on demand.
    pub knowledge: std::sync::Arc<skills::KnowledgeState>,
}

impl Default for ToolState {
    fn default() -> Self {
        Self {
            workspace: Mutex::new(None),
            mcp_servers: Mutex::new(HashMap::new()),
            id: AtomicU64::new(1),
            permission_requests: Mutex::new(HashMap::new()),
            request_id: AtomicU64::new(1),
            knowledge: std::sync::Arc::new(skills::KnowledgeState::default()),
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
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
