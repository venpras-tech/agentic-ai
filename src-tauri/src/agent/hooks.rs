//! Workspace hooks (Claude PreToolUse parity, P3.1).
//!
//! `.ai/hooks.json` in the workspace root declares shell commands the agent
//! runs at lifecycle points. Every hook is executed with the workspace as its
//! cwd and receives a JSON payload on stdin:
//!
//! ```json
//! {
//!   "hooks": {
//!     "pre_tool_use":  [{ "command": "node check.js", "matcher": "write_file|apply_diff", "timeoutMs": 5000 }],
//!     "post_tool_use": [{ "command": "node log.js" }],
//!     "session_start": [{ "command": "node init.js" }]
//!   }
//! }
//! ```
//!
//! Payload fields: `hook_event_name`, `session_id`, `workspace`, and for tool
//! events `tool_name` + `tool_input` (the serialized [`ToolCall`]);
//! `post_tool_use` additionally carries `tool_output`.
//!
//! A `pre_tool_use` hook blocks the call by printing
//! `{"decision":"deny","reason":"..."}` on stdout or by exiting with code 2
//! (stderr becomes the reason). Everything else — empty output, unparsable
//! JSON, spawn failure, timeout — fails open so a broken hook can never brick
//! the agent. `matcher` is a regex on the tool name (absent = every tool).

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use regex::Regex;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use super::ToolCall;

/// Default per-hook wall-clock budget.
const DEFAULT_TIMEOUT_MS: u64 = 10_000;
/// Hard cap so a malicious or buggy hook cannot stall a task indefinitely.
const MAX_TIMEOUT_MS: u64 = 60_000;
/// Cap on tool stdout echoed into `post_tool_use` payloads.
const POST_OUTPUT_LIMIT: usize = 2_000;

#[derive(Debug, Clone, Deserialize)]
pub struct HookSpec {
    pub command: String,
    /// Regex matched against the tool name; absent or empty = every tool.
    #[serde(default)]
    pub matcher: Option<String>,
    #[serde(default, rename = "timeoutMs")]
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
pub struct HooksConfig {
    #[serde(default, rename = "pre_tool_use")]
    pub pre_tool_use: Vec<HookSpec>,
    #[serde(default, rename = "post_tool_use")]
    pub post_tool_use: Vec<HookSpec>,
    #[serde(default, rename = "session_start")]
    pub session_start: Vec<HookSpec>,
}

/// Result of evaluating one or more hooks.
#[derive(Debug, PartialEq, Eq)]
pub enum HookOutcome {
    Pass,
    Deny(String),
}

struct HookOutput {
    stdout: String,
    stderr: String,
    exit_code: Option<i32>,
    timed_out: bool,
    spawn_error: Option<String>,
}

/// Load `.ai/hooks.json` from the workspace root. A missing or unparsable
/// file yields an empty config (fail open) with a log line for the latter.
pub fn load(workspace: &Path) -> HooksConfig {
    let path = workspace.join(".ai").join("hooks.json");
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(_) => return HooksConfig::default(),
    };
    match serde_json::from_str::<HooksConfig>(&raw) {
        Ok(cfg) => cfg,
        Err(e) => {
            crate::logging::warn(None, "hooks", &format!("invalid hooks.json: {e}"));
            HooksConfig::default()
        }
    }
}

/// Build the JSON payload handed to a hook on stdin.
pub fn payload(event: &str, workspace: &Path, call: Option<&ToolCall>, session_id: u64) -> Value {
    let mut v = json!({
        "hook_event_name": event,
        "session_id": session_id,
        "workspace": workspace.display().to_string(),
    });
    if let Some(call) = call {
        v["tool_name"] = json!(call.name());
        v["tool_input"] = serde_json::to_value(call).unwrap_or(Value::Null);
    }
    v
}

/// Attach a completed tool's outcome to a `post_tool_use` payload.
pub fn with_tool_output(
    mut payload: Value,
    success: bool,
    error: Option<&str>,
    stdout: Option<&str>,
) -> Value {
    let out = stdout
        .map(|s| {
            let trimmed: String = s.chars().take(POST_OUTPUT_LIMIT).collect();
            Value::String(trimmed)
        })
        .unwrap_or(Value::Null);
    payload["tool_output"] = json!({
        "success": success,
        "error": error.map(|e| Value::String(e.to_string())).unwrap_or(Value::Null),
        "stdout": out,
    });
    payload
}

/// Run every matching hook in `specs` in order. The first explicit deny wins;
/// passes and failures fall through to the next hook.
pub async fn run_hooks(
    workspace: &Path,
    specs: &[HookSpec],
    payload: &Value,
    tool_name: Option<&str>,
) -> HookOutcome {
    for spec in specs {
        if let Some(pat) = spec.matcher.as_deref().filter(|p| !p.trim().is_empty()) {
            let Some(name) = tool_name else { continue };
            match Regex::new(pat) {
                Ok(re) if re.is_match(name) => {}
                Ok(_) => continue,
                Err(e) => {
                    crate::logging::warn(None, "hooks", &format!("invalid matcher `{pat}`: {e}"));
                    continue;
                }
            }
        }
        let out = run_hook(workspace, spec, payload).await;
        if let HookOutcome::Deny(reason) = decide(&out) {
            return HookOutcome::Deny(reason);
        }
    }
    HookOutcome::Pass
}

/// Run `session_start` hooks and collect their combined stdout (trimmed
/// non-empty chunks joined by newlines). Deny is not meaningful here.
pub async fn run_session_start(
    workspace: &Path,
    specs: &[HookSpec],
    payload: &Value,
) -> Option<String> {
    let mut chunks: Vec<String> = Vec::new();
    for spec in specs {
        let out = run_hook(workspace, spec, payload).await;
        if let Some(e) = &out.spawn_error {
            crate::logging::warn(None, "hooks", &format!("session_start hook failed: {e}"));
        }
        let stdout = out.stdout.trim();
        if !stdout.is_empty() {
            chunks.push(stdout.to_string());
        }
    }
    if chunks.is_empty() {
        None
    } else {
        Some(chunks.join("\n"))
    }
}

/// Interpret one hook run: only an explicit deny (exit 2 or
/// `{"decision":"deny"}`) blocks; every other outcome fails open.
fn decide(out: &HookOutput) -> HookOutcome {
    if let Some(e) = &out.spawn_error {
        crate::logging::warn(None, "hooks", &format!("hook spawn failed: {e}"));
        return HookOutcome::Pass;
    }
    if out.timed_out {
        crate::logging::warn(None, "hooks", "hook timed out — ignored (fail-open)");
        return HookOutcome::Pass;
    }
    if out.exit_code == Some(2) {
        let reason = if !out.stderr.trim().is_empty() {
            out.stderr.trim().to_string()
        } else {
            "blocked by a pre_tool_use hook (exit code 2)".to_string()
        };
        return HookOutcome::Deny(reason);
    }
    if let Ok(v) = serde_json::from_str::<Value>(&out.stdout) {
        if v.get("decision").and_then(|d| d.as_str()) == Some("deny") {
            let reason = v
                .get("reason")
                .and_then(|r| r.as_str())
                .map(str::to_string)
                .unwrap_or_else(|| "blocked by a pre_tool_use hook".to_string());
            return HookOutcome::Deny(reason);
        }
    }
    HookOutcome::Pass
}

/// Spawn one hook command with the payload on stdin; capture stdout/stderr
/// and the exit code. Runs through the platform shell (`cmd /C` / `sh -c`).
async fn run_hook(workspace: &Path, spec: &HookSpec, payload: &Value) -> HookOutput {
    let timeout_ms = spec
        .timeout_ms
        .unwrap_or(DEFAULT_TIMEOUT_MS)
        .clamp(1, MAX_TIMEOUT_MS);
    let mut cmd = if cfg!(windows) {
        let mut c = Command::new("cmd");
        c.arg("/C").arg(&spec.command);
        c
    } else {
        let mut c = Command::new("sh");
        c.arg("-c").arg(&spec.command);
        c
    };
    cmd.current_dir(workspace)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return HookOutput {
                stdout: String::new(),
                stderr: String::new(),
                exit_code: None,
                timed_out: false,
                spawn_error: Some(e.to_string()),
            }
        }
    };

    if let Some(mut stdin) = child.stdin.take() {
        let bytes = payload.to_string().into_bytes();
        let _ = stdin.write_all(&bytes).await;
        let _ = stdin.shutdown().await;
    }

    match tokio::time::timeout(Duration::from_millis(timeout_ms), child.wait_with_output()).await {
        Ok(Ok(output)) => HookOutput {
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            exit_code: output.status.code(),
            timed_out: false,
            spawn_error: None,
        },
        Ok(Err(e)) => HookOutput {
            stdout: String::new(),
            stderr: String::new(),
            exit_code: None,
            timed_out: false,
            spawn_error: Some(e.to_string()),
        },
        // Timed out: the child is dropped (kill_on_drop) and the hook ignored.
        Err(_) => HookOutput {
            stdout: String::new(),
            stderr: String::new(),
            exit_code: None,
            timed_out: true,
            spawn_error: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(command: &str) -> HookSpec {
        HookSpec {
            command: command.to_string(),
            matcher: None,
            timeout_ms: Some(5_000),
        }
    }

    fn temp_workspace(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ai-hooks-test-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(dir.join(".ai")).unwrap();
        dir
    }

    #[test]
    fn parses_hooks_config() {
        let cfg: HooksConfig = serde_json::from_str(
            r#"{
                "pre_tool_use":  [{"command": "a", "matcher": "write_file", "timeoutMs": 1234}],
                "post_tool_use": [{"command": "b"}],
                "session_start": [{"command": "c"}]
            }"#,
        )
        .unwrap();
        assert_eq!(cfg.pre_tool_use.len(), 1);
        assert_eq!(cfg.pre_tool_use[0].timeout_ms, Some(1234));
        assert_eq!(cfg.pre_tool_use[0].matcher.as_deref(), Some("write_file"));
        assert_eq!(cfg.post_tool_use.len(), 1);
        assert_eq!(cfg.session_start.len(), 1);
    }

    #[test]
    fn missing_file_yields_empty_config() {
        let dir = temp_workspace("missing");
        let cfg = load(&dir);
        assert!(cfg.pre_tool_use.is_empty());
        assert!(cfg.post_tool_use.is_empty());
        assert!(cfg.session_start.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn invalid_json_yields_empty_config() {
        let dir = temp_workspace("invalid");
        std::fs::write(dir.join(".ai").join("hooks.json"), "{not json").unwrap();
        let cfg = load(&dir);
        assert!(cfg.pre_tool_use.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn decide_denies_on_exit_code_two() {
        let out = HookOutput {
            stdout: String::new(),
            stderr: "no writes allowed".into(),
            exit_code: Some(2),
            timed_out: false,
            spawn_error: None,
        };
        assert_eq!(decide(&out), HookOutcome::Deny("no writes allowed".into()));
    }

    #[test]
    fn decide_denies_on_json_deny() {
        let out = HookOutput {
            stdout: r#"{"decision":"deny","reason":"policy says no"}"#.into(),
            stderr: String::new(),
            exit_code: Some(0),
            timed_out: false,
            spawn_error: None,
        };
        assert_eq!(decide(&out), HookOutcome::Deny("policy says no".into()));
    }

    #[test]
    fn decide_fails_open_on_garbage_timeout_and_spawn_error() {
        let garbage = HookOutput {
            stdout: "not json".into(),
            stderr: String::new(),
            exit_code: Some(0),
            timed_out: false,
            spawn_error: None,
        };
        assert_eq!(decide(&garbage), HookOutcome::Pass);
        let timed = HookOutput {
            timed_out: true,
            ..garbage
        };
        assert_eq!(decide(&timed), HookOutcome::Pass);
        let spawn = HookOutput {
            stdout: String::new(),
            stderr: String::new(),
            exit_code: None,
            timed_out: false,
            spawn_error: Some("boom".into()),
        };
        assert_eq!(decide(&spawn), HookOutcome::Pass);
    }

    #[test]
    fn matcher_filters_tools() {
        let dir = temp_workspace("matcher");
        let payload = payload("pre_tool_use", &dir, None, 1);
        let mut s = spec("exit 2");
        s.matcher = Some("read_file_range".into());
        // The hook would deny (exit 2) but its matcher excludes this tool.
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let outcome = rt.block_on(run_hooks(
            &dir,
            std::slice::from_ref(&s),
            &payload,
            Some("write_file"),
        ));
        assert_eq!(outcome, HookOutcome::Pass);
        // Matching tool is denied.
        let outcome = rt.block_on(run_hooks(
            &dir,
            std::slice::from_ref(&s),
            &payload,
            Some("read_file_range"),
        ));
        assert_eq!(
            outcome,
            HookOutcome::Deny("blocked by a pre_tool_use hook (exit code 2)".into())
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn live_hook_exit_code_two_denies() {
        let dir = temp_workspace("live-deny");
        let payload = payload("pre_tool_use", &dir, None, 7);
        let outcome = run_hooks(&dir, &[spec("exit 2")], &payload, Some("write_file")).await;
        assert_eq!(
            outcome,
            HookOutcome::Deny("blocked by a pre_tool_use hook (exit code 2)".into())
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn live_hook_pass_when_exit_zero() {
        let dir = temp_workspace("live-pass");
        let payload = payload("pre_tool_use", &dir, None, 7);
        let outcome = run_hooks(&dir, &[spec("exit 0")], &payload, Some("write_file")).await;
        assert_eq!(outcome, HookOutcome::Pass);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn session_start_collects_stdout() {
        let dir = temp_workspace("session-start");
        let p = payload("session_start", &dir, None, 3);
        let out = run_session_start(&dir, &[spec("echo hello-hooks")], &p).await;
        assert_eq!(out.as_deref(), Some("hello-hooks"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
