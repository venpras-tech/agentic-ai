//! Persistent interactive terminal sessions (integrated terminal pane).
//!
//! A terminal is a long-lived shell process (`cmd` on Windows, `sh` elsewhere)
//! that stays running between commands, so state like the current directory
//! and exported env vars persists across inputs — unlike the one-shot
//! `execute_terminal_command` tool. Output is streamed to the frontend as
//! `agent://terminal-output` events.
//!
//! This is a *line-oriented* interactive shell, not a raw PTY: we only handle
//! newline-delimited input/output and don't interpret terminal escape sequences
//! or raw mode. It is deliberately free of native TTY dependencies.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex};

use serde::Serialize;
use tauri::{AppHandle, Emitter};
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::process::Child;

/// Output from a terminal, pushed to the frontend as an event.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalOutputEvent {
    pub id: String,
    /// Raw chunk of stdout/stderr.
    pub data: String,
    /// Stream that produced the chunk: "stdout" | "stderr".
    pub stream: String,
    /// Present once when the process exits.
    pub exit_code: Option<i32>,
}

struct Session {
    id: String,
    /// `None` once the process has exited.
    child: Option<Child>,
    cwd: PathBuf,
}

/// Registry of live terminal sessions. Writes to `child.stdin` happen on the
/// command-handler thread; output pumps read stdout/stderr on background tasks.
pub struct TerminalSessions {
    inner: Mutex<HashMap<String, Session>>,
}

impl Default for TerminalSessions {
    fn default() -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
        }
    }
}

impl TerminalSessions {
    pub fn new() -> Self {
        Self::default()
    }

    /// Spawn a persistent shell in `cwd` and stream output to the frontend.
    /// `me` is an `Arc` to this registry so background tasks can clean up the
    /// entry once the process exits.
    pub fn spawn(
        self: &Arc<Self>,
        app: &AppHandle,
        cwd: Option<String>,
    ) -> Result<String, String> {
        let id = uuid();
        let cwd = cwd
            .map(PathBuf::from)
            .filter(|p| p.is_dir())
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

        let mut cmd = tokio::process::Command::new(shell());
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .current_dir(&cwd)
            .kill_on_drop(true);
        #[cfg(windows)]
        {
            cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }

        let mut child = cmd
            .spawn()
            .map_err(|e| format!("Failed to start terminal shell: {e}"))?;

        let stdout = child.stdout.take().expect("stdout piped");
        let stderr = child.stderr.take().expect("stderr piped");

        // Store a copy of the child (the one we keep) so `write` can access
        // stdin. We take stdin out here and keep it in the session.
        let session = Session {
            id: id.clone(),
            child: Some(child),
            cwd: cwd.clone(),
        };
        {
            let mut map = self.inner.lock().unwrap();
            map.insert(id.clone(), session);
        }

        // Output pumps run on a native thread (Tauri commands run on the Tokio
        // runtime, so `Handle::current().block_on` is available). Each pump
        // forwards a stream's lines to the frontend until EOF.
        let app_out = app.clone();
        let app_err = app.clone();
        let id_out = id.clone();
        let id_err = id.clone();

        std::thread::spawn(move || {
            let rt = tokio::runtime::Handle::current();
            rt.block_on(async move {
                let out_app = app_out.clone();
                let eout = id_out;
                tokio::spawn(async move {
                    pump(out_app, eout.clone(), "stdout", BufReader::new(stdout)).await;
                });
                let err_app = app_err.clone();
                let eerr = id_err;
                tokio::spawn(async move {
                    pump(err_app, eerr.clone(), "stderr", BufReader::new(stderr)).await;
                });
            });
        });

        Ok(id)
    }

    /// Write a single line (plus newline) to the terminal's stdin.
    pub fn write(&self, id: &str, line: &str) -> Result<(), String> {
        let mut map = self.inner.lock().unwrap();
        let session = map
            .get_mut(id)
            .ok_or_else(|| format!("Unknown terminal: {id}"))?;
        let child = session
            .child
            .as_mut()
            .ok_or_else(|| format!("Terminal {id} has exited"))?;
        let stdin = child
            .stdin
            .as_mut()
            .ok_or_else(|| "Terminal stdin unavailable".to_string())?;
        let mut line = line.to_string();
        line.push('\n');
        tokio::runtime::Handle::current()
            .block_on(stdin.write_all(line.as_bytes()))
            .map_err(|e| format!("Failed to write to terminal: {e}"))?;
        Ok(())
    }

    /// Terminate a terminal session (kills the process tree).
    pub fn kill(&self, id: &str) -> Result<(), String> {
        let mut map = self.inner.lock().unwrap();
        if let Some(session) = map.remove(id) {
            if let Some(mut child) = session.child {
                if let Some(pid) = child.id() {
                    let _ = kill_tree(pid);
                }
                let _ = child.kill();
            }
        }
        Ok(())
    }

    /// List live terminal ids + their cwd.
    pub fn list(&self) -> Vec<TerminalInfo> {
        let map = self.inner.lock().unwrap();
        map.values()
            .map(|s| TerminalInfo {
                id: s.id.clone(),
                cwd: s.cwd.display().to_string(),
            })
            .collect()
    }
}

async fn pump<R: tokio::io::AsyncRead + Unpin>(
    app: tauri::AppHandle,
    id: String,
    stream: &'static str,
    mut reader: BufReader<R>,
) {
    use tokio::io::AsyncBufReadExt;
    let mut lines = reader.lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let _ = app.emit(
            "agent://terminal-output",
            TerminalOutputEvent {
                id: id.clone(),
                data: format!("{line}\n"),
                stream: stream.into(),
                exit_code: None,
            },
        );
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalInfo {
    pub id: String,
    pub cwd: String,
}

fn shell() -> &'static str {
    if cfg!(windows) {
        "cmd"
    } else {
        "sh"
    }
}

fn kill_tree(pid: u32) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        std::process::Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
    }
    #[cfg(not(windows))]
    {
        std::process::Command::new("kill")
            .args(["-9", &pid.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
    }
    Ok(())
}

fn uuid() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("term-{nanos:x}-{:016x}", (std::process::id() as u64) | randish())
}

fn randish() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64 ^ d.as_secs())
        .unwrap_or(1)
}
