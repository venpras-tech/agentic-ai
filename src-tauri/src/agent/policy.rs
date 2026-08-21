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
        _ => {
            // Session memory: a tool the user allowed "for this session" skips
            // the ask entirely (the memory is not written to policy.json).
            if session_remembered(state, call) {
                return Verdict::Allow;
            }
            Verdict::Ask {
                request_id: state.next_request_id(),
            }
        }
    }
}

/// Memory key for a call: terminal commands are keyed by the exact command (one
/// approved `cargo test` never silently unlocks a different command); all other
/// tools are keyed by tool name.
pub fn session_key(call: &ToolCall) -> String {
    match call {
        ToolCall::ExecuteTerminalCommand { command, .. } => {
            format!("{}:{command}", call.name())
        }
        _ => call.name().to_string(),
    }
}

/// Remember that `call` is allowed for the rest of this app session.
pub fn remember_session(state: &super::ToolState, call: &ToolCall) {
    state.session_allow.lock().unwrap().insert(session_key(call));
}

fn session_remembered(state: &super::ToolState, call: &ToolCall) -> bool {
    state
        .session_allow
        .lock()
        .unwrap()
        .contains(&session_key(call))
}

/// Persist "always allow" for `call` by writing an `allow` rule into the
/// workspace's `.ai/policy.json` (merging with any existing rules).
pub fn remember_always(workspace: Option<&Path>, call: &ToolCall) -> Result<(), String> {
    let Some(ws) = workspace else {
        return Ok(());
    };
    let dir = ws.join(".ai");
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("Failed to create {}: {e}", dir.display()))?;
    let path = dir.join("policy.json");
    let mut cfg = load_policy(Some(ws));
    let tool = call.name().to_string();
    // The injected red-zone rule must never be persisted back to disk.
    let (rule_tool, command_patterns) = match call {
        ToolCall::ExecuteTerminalCommand { command, .. } => (tool.clone(), vec![command.clone()]),
        _ => (tool.clone(), Vec::new()),
    };
    cfg.rules.retain(|r| {
        r.tool != "__red_zone__" && !(r.tool == rule_tool && r.command_patterns == command_patterns)
    });
    cfg.rules.push(PolicyRule {
        tool: rule_tool,
        policy: "allow".into(),
        command_patterns,
    });
    let text = serde_json::to_string_pretty(&cfg)
        .map_err(|e| format!("Failed to serialize policy: {e}"))?;
    std::fs::write(&path, text).map_err(|e| format!("Failed to write {}: {e}", path.display()))
}

/// Read-only tools never need approval (default allow) unless policy overrides.
pub fn default_allow(tool: &str) -> bool {
    matches!(
        tool,
        "glob_search_codebase"
            | "search_file_contents"
            | "semantic_search_codebase"
            | "view_file_structure"
            | "read_file_range"
            | "read_skill"
            | "read_plan"
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
