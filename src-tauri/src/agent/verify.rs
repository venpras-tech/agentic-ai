//! Post-edit verify harness: runs an explicit `.ai/verify.json` command or a
//! per-language default and reports the outcome. Output is always text — a
//! failed spawn / timeout / non-zero exit is reported, never propagated.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;

/// Schema for `{workspace}/.ai/verify.json`.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct VerifyConfig {
    pub command: String,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

/// Outcome of one verify process (or the aggregate for a verify pass).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifyRun {
    pub ok: bool,
    /// Human-readable command that ran (empty for LSP-only passes).
    pub command: String,
    pub exit_code: Option<i32>,
    pub report: String,
}

/// Read `.ai/verify.json` from `workspace`. Returns `None` when the file is
/// absent, unparseable, or has a blank command.
pub fn load_verify_config(workspace: &Path) -> Option<VerifyConfig> {
    let raw = std::fs::read_to_string(workspace.join(".ai").join("verify.json")).ok()?;
    let cfg: VerifyConfig = serde_json::from_str(&raw).ok()?;
    if cfg.command.trim().is_empty() {
        return None;
    }
    Some(cfg)
}

impl VerifyConfig {
    /// `cwd` relative to the workspace (absolute paths are used as-is).
    pub fn resolved_cwd(&self, workspace: &Path) -> PathBuf {
        match self.cwd.as_deref() {
            Some(c) if !c.trim().is_empty() => {
                let p = PathBuf::from(c);
                if p.is_absolute() {
                    p
                } else {
                    workspace.join(p)
                }
            }
            _ => workspace.to_path_buf(),
        }
    }

    pub fn timeout(&self) -> Duration {
        Duration::from_secs(self.timeout_secs.unwrap_or(120))
    }

    /// Wrap the configured command string in the platform shell.
    pub fn shell_command(&self) -> (&'static str, Vec<String>) {
        if cfg!(windows) {
            ("cmd", vec!["/C".into(), self.command.clone()])
        } else {
            ("sh", vec!["-c".into(), self.command.clone()])
        }
    }
}

/// Per-language default verify command for the edited files, or `None` when no
/// manifest / toolchain marker matches.
pub fn default_command(
    workspace: &Path,
    edited_files: &[String],
) -> Option<(&'static str, Vec<String>)> {
    let has_rs = edited_files.iter().any(|p| p.ends_with(".rs"));
    let has_ts = edited_files.iter().any(|p| {
        p.ends_with(".ts") || p.ends_with(".tsx") || p.ends_with(".js") || p.ends_with(".jsx")
    });
    let has_py = edited_files.iter().any(|p| p.ends_with(".py"));

    if has_rs && workspace.join("Cargo.toml").is_file() {
        Some(("cargo", vec!["check".into()]))
    } else if has_ts && workspace.join("tsconfig.json").is_file() {
        Some(("npx", vec!["tsc".into(), "--noEmit".into()]))
    } else if has_py {
        let mut args = vec!["-m".into(), "py_compile".into()];
        args.extend(edited_files.iter().cloned());
        Some(("python", args))
    } else {
        None
    }
}

/// Run `program args` in `cwd`, capturing combined output within `timeout`.
pub async fn run_process(
    program: &str,
    args: &[String],
    cwd: &Path,
    timeout: Duration,
) -> VerifyRun {
    let display = format!("{program} {}", args.join(" "));
    let mut cmd = tokio::process::Command::new(program);
    cmd.args(args)
        .current_dir(cwd)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    {
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let output = match tokio::time::timeout(timeout, cmd.output()).await {
        Ok(Ok(out)) => out,
        Ok(Err(e)) => {
            return VerifyRun {
                ok: false,
                command: display,
                exit_code: None,
                report: format!("could not start `{program}` (is it installed?): {e}"),
            }
        }
        Err(_) => {
            return VerifyRun {
                ok: false,
                command: display,
                exit_code: None,
                report: format!("`{program}` timed out after {timeout:?}"),
            }
        }
    };
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let success = output.status.success();
    let exit_code = output.status.code();
    let mut report = format!("ran `{display}` (exit {})", exit_code.unwrap_or(-1));
    if !stdout.is_empty() {
        report.push_str("\n--- stdout ---\n");
        report.push_str(&truncate(&stdout));
    }
    if !stderr.is_empty() {
        report.push_str("\n--- stderr ---\n");
        report.push_str(&truncate(&stderr));
    }
    report.push_str(if success {
        "\n(no errors detected)"
    } else {
        "\n(verify found errors/warnings — see above)"
    });
    VerifyRun {
        ok: success,
        command: display,
        exit_code,
        report,
    }
}

/// Run the verify pass for `workspace`: `.ai/verify.json` if present, else a
/// per-language default, plus LSP diagnostics for the edited files. Returns
/// `None` when nothing applies (no command and no diagnostics).
pub async fn run(
    workspace: &Path,
    edited_files: &[String],
    lsp: Option<&std::sync::Arc<crate::lsp::LspManager>>,
) -> Option<VerifyRun> {
    let mut parts: Vec<String> = Vec::new();
    let mut ok = true;
    let mut command = String::new();
    let mut exit_code = None;

    if let Some(cfg) = load_verify_config(workspace) {
        let (program, args) = cfg.shell_command();
        let run = run_process(program, &args, &cfg.resolved_cwd(workspace), cfg.timeout()).await;
        ok &= run.ok;
        command = cfg.command.clone();
        exit_code = run.exit_code;
        parts.push(run.report);
    } else if let Some((program, args)) = default_command(workspace, edited_files) {
        let run = run_process(program, &args, workspace, Duration::from_secs(60)).await;
        ok &= run.ok;
        command = run.command.clone();
        exit_code = run.exit_code;
        parts.push(run.report);
    }

    if let Some(lsp) = lsp {
        let lines: Vec<String> = edited_files
            .iter()
            .filter_map(|p| lsp.diagnostics_path(Path::new(p)))
            .flat_map(lsp_diag_lines)
            .collect();
        if !lines.is_empty() {
            ok = false;
            let mut block = "LSP diagnostics for edited files:".to_string();
            for l in lines {
                block.push('\n');
                block.push_str(&l);
            }
            parts.push(block);
        }
    }

    if parts.is_empty() {
        None
    } else {
        Some(VerifyRun {
            ok,
            command,
            exit_code,
            report: parts.join("\n"),
        })
    }
}

/// One line per finding from a resolved [`crate::lsp::LspDiagnostics`] probe,
/// formatted for the verify report. Empty when the server reported nothing
/// (clean file skips the block entirely).
pub fn lsp_diag_lines(res: crate::lsp::LspDiagnostics) -> Vec<String> {
    let Some(server) = res.server.as_deref() else {
        return Vec::new();
    };
    let mut lines = Vec::new();
    for d in res.diagnostics {
        let rel = std::path::Path::new(&d.path)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| d.path.clone());
        let code = d
            .code
            .as_deref()
            .map(|c| format!(" ({c})"))
            .unwrap_or_default();
        lines.push(format!(
            "- {rel}:{line}:{col} [{severity}] {message}{code} (via {server})",
            rel = rel,
            line = d.line,
            col = d.column,
            severity = d.severity,
            message = d.message,
            code = code,
            server = server,
        ));
    }
    lines
}

fn truncate(s: &str) -> String {
    const LIMIT: usize = 4000;
    if s.len() <= LIMIT {
        return s.to_string();
    }
    let mut end = LIMIT;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}… [truncated]", &s[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_parses_camel_case_and_defaults() {
        let cfg: VerifyConfig =
            serde_json::from_str(r#"{ "command": "npm test", "cwd": ".", "timeoutSecs": 30 }"#)
                .unwrap();
        assert_eq!(cfg.command, "npm test");
        assert_eq!(
            cfg.resolved_cwd(Path::new("D:/ws")),
            PathBuf::from("D:/ws/.")
        );
        assert_eq!(cfg.timeout(), Duration::from_secs(30));

        let bare: VerifyConfig = serde_json::from_str(r#"{ "command": "make" }"#).unwrap();
        assert_eq!(bare.cwd, None);
        assert_eq!(bare.timeout(), Duration::from_secs(120));
        assert_eq!(
            bare.resolved_cwd(Path::new("D:/ws")),
            PathBuf::from("D:/ws")
        );
    }

    #[test]
    fn load_verify_config_reads_and_rejects_blank() {
        let root = std::env::temp_dir().join(format!("verify-cfg-{}", std::process::id()));
        let ai = root.join(".ai");
        std::fs::create_dir_all(&ai).unwrap();

        std::fs::write(ai.join("verify.json"), r#"{ "command": "npm test" }"#).unwrap();
        assert_eq!(load_verify_config(&root).unwrap().command, "npm test");

        std::fs::write(ai.join("verify.json"), r#"{ "command": "  " }"#).unwrap();
        assert!(load_verify_config(&root).is_none());

        std::fs::write(ai.join("verify.json"), "not json").unwrap();
        assert!(load_verify_config(&root).is_none());

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn default_command_matches_by_language_and_manifest() {
        let root = std::env::temp_dir().join(format!("verify-def-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("Cargo.toml"), "[package]").unwrap();

        assert_eq!(
            default_command(&root, &["src/main.rs".into()]),
            Some(("cargo", vec!["check".to_string()]))
        );
        assert!(default_command(&root, &["README.md".into()]).is_none());

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn lsp_diag_lines_formats_findings_only() {
        let res = crate::lsp::LspDiagnostics {
            server: Some("rust-analyzer".into()),
            diagnostics: vec![crate::lsp::LspDiagnostic {
                path: "src/main.rs".into(),
                line: 2,
                column: 13,
                severity: "error".into(),
                message: "expected expression, found `;`".into(),
                code: Some("syntax-error".into()),
            }],
        };
        let lines = lsp_diag_lines(res);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("main.rs:2:13"));
        assert!(lines[0].contains("[error] expected expression, found `;` (syntax-error)"));
        assert!(lines[0].contains("(via rust-analyzer)"));

        let none = lsp_diag_lines(crate::lsp::LspDiagnostics {
            server: None,
            diagnostics: Vec::new(),
        });
        assert!(none.is_empty());

        let clean = lsp_diag_lines(crate::lsp::LspDiagnostics {
            server: Some("rust-analyzer".into()),
            diagnostics: Vec::new(),
        });
        assert!(clean.is_empty());
    }
}
