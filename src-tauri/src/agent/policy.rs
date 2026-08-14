//! Permission & safety model: per-tool allow/ask/deny, red-zone commands and
//! workspace scoping.
//!
//! Policy is configured in `{workspace}/.ai/policy.json`:
//! ```json
//! {
//!   "default": "ask",
//!   "rules": [
//!     { "tool": "execute_terminal_command", "policy": "allow",
//!       "commandPatterns": ["npm test", "cargo check", "cargo test", "git status", "git diff"] },
//!     { "tool": "apply_file_diff", "policy": "ask" }
//!   ]
//! }
//! ```
//!
//! * Read-only tools default to **allow**.
//! * Mutating tools (file writes, git commits, MCP) default to **ask** — the
//!   frontend must approve before the tool runs.
//! * Red-zone commands (`rm -rf /`, `git push --force`, …) are **always denied**
//!   regardless of any allow rule.

use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::ToolCall;

/// Per-tool verdict for a single call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Allow,
    Deny(String),
    Ask { request_id: String },
}

/// Configured policy for one tool.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PolicyRule {
    pub tool: String,
    #[serde(default = "default_policy_string")]
    pub policy: String, // "allow" | "ask" | "deny"
    #[serde(default)]
    pub command_patterns: Vec<String>,
}

fn default_policy_string() -> String {
    "ask".into()
}

/// Root policy document.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PolicyConfig {
    #[serde(default = "default_policy_string")]
    pub default: String,
    #[serde(default)]
    pub rules: Vec<PolicyRule>,
}

impl Default for PolicyConfig {
    fn default() -> Self {
        Self {
            default: "ask".into(),
            rules: Vec::new(),
        }
    }
}

/// Read the workspace policy file, or the sensible default when missing.
pub fn load_policy(workspace: Option<&Path>) -> PolicyConfig {
    let mut cfg = match workspace {
        Some(ws) => match std::fs::read_to_string(ws.join(".ai/policy.json")) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
            Err(_) => PolicyConfig::default(),
        },
        None => PolicyConfig::default(),
    };
    // Always enforce the red zone first, even if the file says allow.
    cfg.rules.insert(0, PolicyRule {
        tool: "__red_zone__".into(),
        policy: "deny".into(),
        command_patterns: red_zone_patterns(),
    });
    cfg
}

/// Substrings that flag a command as destructive / irreversible. Matched
/// case-insensitively against the raw command string.
fn red_zone_patterns() -> Vec<String> {
    [
        "rm -rf /",
        "rm -rf c:\\",
        "rm -r /",
        "del /s /q c:",
        "del /s /q c:\\",
        "format c:",
        "format c:\\",
        "git push --force",
        "git push -f",
        "git reset --hard",
        "git clean -f",
        "git clean -fd",
        "git checkout .",
        "mkfs",
        "fdisk",
        "dd if=/dev/zero",
        "shutdown",
        "reboot",
        "> /dev/sda",
        ":(){ :|:& };:",
        "rd /s /q c:",
        "rmdir /s /q c:",
        "powershell -c clear-disk",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

/// Check a tool call against the policy. `workspace` enables path scoping.
pub fn check(state: &super::ToolState, call: &ToolCall, workspace: Option<&Path>) -> Verdict {
    // 1. Workspace path scoping for file tools.
    if let Some(path) = call_target_path(call) {
        if let Some(ws) = workspace {
            if !is_within(Path::new(path), ws) {
                return Verdict::Deny(format!(
                    "Path `{path}` is outside the workspace `{}`. Refusing to touch files outside the project.",
                    ws.display()
                ));
            }
        }
    }

    let cfg = load_policy(workspace);
    let tool = call.name();

    // 2. Red-zone command detection (denied unconditionally).
    if let ToolCall::ExecuteTerminalCommand { command, .. } = call {
        let lower = command.to_lowercase();
        for pattern in &cfg.rules.iter().find(|r| r.tool == "__red_zone__").unwrap().command_patterns {
            if lower.contains(&pattern.to_lowercase()) {
                return Verdict::Deny(format!(
                    "Command matches a red-zone pattern (`{pattern}`) and is always blocked."
                ));
            }
        }
    }

    // 3. Tool-level policy resolution.
    let policy = match cfg
        .rules
        .iter()
        .filter(|r| r.tool == tool)
        .find(|r| match call {
            ToolCall::ExecuteTerminalCommand { command, .. } => {
                r.command_patterns.is_empty()
                    || r.command_patterns
                        .iter()
                        .any(|p| command.contains(p.as_str()))
            }
            _ => true,
        }) {
        Some(r) => r.policy.as_str(),
        None => {
            // Read-only tools default to allow unless explicitly overridden.
            if default_allow(tool) {
                return Verdict::Allow;
            }
            &cfg.default
        }
    };

    match policy {
        "allow" => Verdict::Allow,
        "deny" => Verdict::Deny(format!(
            "Tool `{tool}` is blocked by the workspace policy (.ai/policy.json)."
        )),
        _ => Verdict::Ask {
            request_id: state.next_request_id(),
        },
    }
}

/// Read-only tools never need approval (default allow) unless policy overrides.
pub fn default_allow(tool: &str) -> bool {
    matches!(
        tool,
        "glob_search_codebase"
            | "search_file_contents"
            | "view_file_structure"
            | "read_file_range"
            | "read_skill"
            | "git_status"
            | "git_diff"
            | "run_tests"
    )
}

/// Return the path a file tool targets (for scoping), if any.
fn call_target_path(call: &ToolCall) -> Option<&str> {
    match call {
        ToolCall::ViewFileStructure { path, .. }
        | ToolCall::ReadFileRange { path, .. }
        | ToolCall::ApplyFileDiff { path, .. }
        | ToolCall::WriteFile { path, .. } => Some(path),
        _ => None,
    }
}

/// Canonicalized prefix check: is `path` inside `root`?
fn is_within(path: &Path, root: &Path) -> bool {
    let canon = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    let p = canon(path);
    let r = canon(root);
    p.starts_with(&r)
}

/// Serialize the effective policy for the UI (read-only snapshot).
pub fn snapshot(workspace: Option<&Path>) -> Value {
    let cfg = load_policy(workspace);
    serde_json::json!({
        "default": cfg.default,
        "rules": cfg.rules,
    })
}
