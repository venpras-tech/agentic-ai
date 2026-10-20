//! Minimal Language Server Protocol client: spawn a per-language server once
//! per project root, open/change documents, and resolve diagnostics for the
//! editor gutter and the post-edit verify pass.
//!
//! Diagnostics come from `textDocument/publishDiagnostics` (all major servers)
//! with an optional pull via `textDocument/diagnostic` when a server advertises
//! it (rust-analyzer, pyright) for a deterministic probe.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use serde::Serialize;
use serde_json::{json, Value};
use tauri::AppHandle;

use crate::commands::settings::read_settings;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// One diagnostic, display-ready (1-based line/column).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LspDiagnostic {
    pub path: String,
    pub line: usize,
    pub column: usize,
    pub severity: String,
    pub message: String,
    pub code: Option<String>,
}

/// Result of a diagnostics probe: which server produced them (if any).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LspDiagnostics {
    pub server: Option<String>,
    pub diagnostics: Vec<LspDiagnostic>,
}

/// A configured language server: which command to run and for which files.
#[derive(Debug, Clone)]
pub struct LspServerSpec {
    pub key: String,
    pub display: String,
    pub command: String,
    pub args: Vec<String>,
    pub pull: bool,
}

/// Built-in server table (overridable via `lspServers` in settings.json).
const BUILTIN: [(&str, &str, &str, &[&str], bool); 3] = [
    ("rust", "rust-analyzer", "rust-analyzer", &[], true),
    (
        "typescript",
        "typescript-language-server",
        "typescript-language-server",
        &["--stdio"],
        false,
    ),
    (
        "python",
        "pyright-langserver",
        "pyright-langserver",
        &["--stdio"],
        true,
    ),
];

/// Map a file extension onto a language key, or `None` when nothing is built
/// in (or configured) for that file type.
pub fn language_for(path: &Path) -> Option<&'static str> {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("rs") => Some("rust"),
        Some("ts" | "tsx" | "js" | "jsx" | "mts" | "cts" | "mjs" | "cjs") => Some("typescript"),
        Some("py" | "pyi") => Some("python"),
        _ => None,
    }
}

/// Built-in spec for a language, as a starting point for `spec_for` overrides.
fn builtin_spec(key: &str) -> Option<LspServerSpec> {
    let (_, display, command, args, pull) = BUILTIN.iter().find(|(k, ..)| *k == key)?;
    Some(LspServerSpec {
        key: key.to_string(),
        display: (*display).to_string(),
        command: (*command).to_string(),
        args: args.iter().map(|s| s.to_string()).collect(),
        pull: *pull,
    })
}

/// Resolve the server for `language`, merging a `lspServers` settings object
/// (per-language: `enabled`, `command`, `args`, `pull`) over the builtin.
pub fn spec_for(app: &AppHandle, language: &str) -> Option<LspServerSpec> {
    let settings = read_settings(app);
    let servers = settings.extra_value("lspServers");
    spec_with_extra(language, servers)
}

/// Pure form of [`spec_for`] — testable without an [`AppHandle`].
fn spec_with_extra(language: &str, servers: Option<&Value>) -> Option<LspServerSpec> {
    let mut spec = builtin_spec(language)?;
    let entry = match servers {
        Some(s) => s.get(language).and_then(Value::as_object),
        None => None,
    };
    let Some(entry) = entry else {
        return Some(spec);
    };
    if entry.get("enabled").and_then(Value::as_bool) == Some(false) {
        return None;
    }
    if let Some(cmd) = entry
        .get("command")
        .and_then(Value::as_str)
        .filter(|c| !c.is_empty())
    {
        spec.command = cmd.to_string();
        spec.display = cmd.to_string();
    }
    if let Some(args) = entry.get("args").and_then(Value::as_array) {
        let flat: Vec<String> = args
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect();
        if !flat.is_empty() {
            spec.args = flat;
        }
    }
    if let Some(pull) = entry.get("pull").and_then(Value::as_bool) {
        spec.pull = pull;
    }
    Some(spec)
}

/// Nearest ancestor directory that looks like the project root for `language`
/// (marker manifest or `.git`), falling back to the file's own directory.
pub fn resolve_root(path: &Path, language: &str) -> PathBuf {
    let mut dir = path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();
    loop {
        let markers: &[&str] = match language {
            "rust" => &["Cargo.toml"],
            "typescript" => &["tsconfig.json", "package.json"],
            "python" => &["pyproject.toml", "setup.py", "requirements.txt"],
            _ => &[],
        };
        if markers.iter().any(|m| dir.join(m).is_file()) || dir.join(".git").is_dir() {
            return dir;
        }
        if !dir.pop() {
            return path
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| PathBuf::from("."));
        }
    }
}

/// Extract one complete `Content-Length`-framed message body from `buf`,
/// leaving the remainder in place. Returns `None` when a full frame is not yet
/// buffered (pure, so framing can be unit-tested).
fn try_take_message(buf: &mut Vec<u8>) -> Option<Vec<u8>> {
    let head_end = find_subslice(buf, b"\r\n\r\n")?;
    let head = String::from_utf8_lossy(&buf[..head_end]);
    let len = head.lines().find_map(|l| {
        let l = l.trim();
        let k = l.to_ascii_lowercase();
        k.strip_prefix("content-length:")?
            .trim()
            .parse::<usize>()
            .ok()
    })?;
    let total = head_end + 4 + len;
    if buf.len() < total {
        return None;
    }
    let body = buf[head_end + 4..total].to_vec();
    buf.drain(..total);
    Some(body)
}

fn find_subslice(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Convert the LSP `range`/`severity` shape into a display diagnostic.
fn canonicalize(uri: &str, path: &str, value: &Value) -> Option<LspDiagnostic> {
    let range = value.get("range")?;
    let start = range.get("start")?;
    let line = start.get("line")?.as_u64()? as usize + 1;
    let column = start.get("character")?.as_u64()? as usize + 1;
    let severity = match value.get("severity").and_then(Value::as_u64).unwrap_or(1) {
        2 => "warning",
        3 => "info",
        4 => "hint",
        _ => "error",
    };
    let message = value
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .chars()
        .take(2000)
        .collect::<String>();
    let _ = uri;
    Some(LspDiagnostic {
        path: path.to_string(),
        line,
        column,
        severity: severity.to_string(),
        message,
        code: value
            .get("code")
            .and_then(Value::as_u64)
            .map(|n| n.to_string())
            .or_else(|| {
                value
                    .get("code")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            }),
    })
}
/// Parse a `textDocument/publishDiagnostics` notification `params` into the
/// canonical `(case-keyed path, diagnostics)` pair stored on the manager. Pure,
/// so the JSON-RPC → `LspDiagnostic` bridge is testable without a live server.
fn parse_publish_diagnostics(params: &Value) -> Option<(String, Vec<LspDiagnostic>)> {
    let uri = params.get("uri").and_then(Value::as_str)?;
    let rel = uri_to_path(uri);
    let key = case_key(&rel);
    let items = params
        .get("diagnostics")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let diags: Vec<LspDiagnostic> = items
        .iter()
        .filter_map(|d| canonicalize(uri, &rel, d))
        .collect();
    Some((key, diags))
}

/// Items array from a `textDocument/diagnostic` pull response.
fn pull_items(result: &Value) -> Vec<Value> {
    match result.get("kind").and_then(Value::as_str) {
        Some("unchanged") => Vec::new(),
        _ => result
            .get("items")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default(),
    }
}

/// A running server connection: stdin/stdout pumped by two OS threads, JSON-RPC
/// responses routed to one-shot waiters, diagnostics stored per URI.
struct LspSession {
    pull: AtomicBool,
    id_seq: AtomicU64,
    #[allow(dead_code)] // held so the server process is reaped / not detached
    child: Mutex<Child>,
    pending: Mutex<HashMap<u64, mpsc::Sender<Value>>>,
    diags: Mutex<HashMap<String, Vec<LspDiagnostic>>>,
    diag_gen: Mutex<HashMap<String, u64>>,
    docs: Mutex<HashMap<String, (i64, Option<String>)>>,
    out_tx: Mutex<Option<mpsc::Sender<Vec<u8>>>>,
    alive: AtomicBool,
    debug: bool,
}

impl LspSession {
    /// Spawn the server, wire the I/O threads, and complete the LSP handshake
    /// (`initialize` request + `initialized` notification).
    fn spawn(spec: &LspServerSpec, root: &Path, path: &Path) -> Result<Arc<Self>, String> {
        let mut cmd = Command::new(&spec.command);
        let debug = std::env::var("AI_EDITOR_LSP_DEBUG").is_ok();
        cmd.args(&spec.args)
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(if debug {
                Stdio::inherit()
            } else {
                Stdio::null()
            });
        #[cfg(windows)]
        cmd.creation_flags(CREATE_NO_WINDOW);
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("could not start `{}` (is it installed?): {e}", spec.command))?;

        let stdin = child.stdin.take().expect("stdin piped");
        let stdout = child.stdout.take().expect("stdout piped");

        let (out_tx, out_rx) = mpsc::channel::<Vec<u8>>();
        let session = Arc::new(LspSession {
            pull: AtomicBool::new(spec.pull),
            id_seq: AtomicU64::new(1),
            child: Mutex::new(child),
            pending: Mutex::new(HashMap::new()),
            diags: Mutex::new(HashMap::new()),
            diag_gen: Mutex::new(HashMap::new()),
            docs: Mutex::new(HashMap::new()),
            out_tx: Mutex::new(Some(out_tx)),
            alive: AtomicBool::new(true),
            debug,
        });

        let s = Arc::clone(&session);
        std::thread::spawn(move || s.writer(stdin, out_rx));
        let s = Arc::clone(&session);
        std::thread::spawn(move || s.reader(stdout));

        let root_uri = file_uri(root);
        let s = Arc::clone(&session);
        let init = s
            .request(
                "initialize",
                json!({
                    "processId": null,
                    "clientInfo": { "name": "ai-editor", "version": env!("CARGO_PKG_VERSION") },
                    "rootUri": root_uri,
                    "rootPath": root.to_string_lossy(),
                    "capabilities": {},
                    "workspaceFolders": [ { "uri": root_uri, "name": root.file_name().and_then(|n| n.to_str()).unwrap_or("root") } ]
                }),
            )
            .map_err(|e| format!("`{}` failed to initialize: {e}", spec.command))?;
        // Prefer `textDocument/diagnostic` whenever the server advertises it.
        // Push diagnostics cannot be trusted for a re-analysis: rust-analyzer
        // reuses the pre-edit findings (and sometimes publishes nothing at all)
        // after `didChange`, whereas a pull recomputes and reports the fix.
        if init
            .get("capabilities")
            .and_then(|c| c.get("diagnosticProvider"))
            .is_some()
        {
            s.pull.store(true, Ordering::SeqCst);
        }
        s.send_message(json!({ "jsonrpc": "2.0", "method": "initialized", "params": {} }));
        let _ = file_uri(path);
        Ok(s)
    }

    fn writer(&self, mut stdin: std::process::ChildStdin, rx: mpsc::Receiver<Vec<u8>>) {
        while let Ok(frame) = rx.recv() {
            if stdin.write_all(&frame).is_err() {
                break;
            }
            let _ = stdin.flush();
        }
    }

    fn reader(&self, mut stdout: std::process::ChildStdout) {
        let mut buf: Vec<u8> = Vec::new();
        let mut chunk = [0u8; 8192];
        loop {
            match stdout.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
                Err(_) => break,
            }
            while let Some(body) = try_take_message(&mut buf) {
                self.handle_message(&body);
            }
        }
        self.alive.store(false, Ordering::SeqCst);
    }

    fn handle_message(&self, body: &[u8]) {
        let value: Value = match serde_json::from_slice(body) {
            Ok(v) => v,
            Err(_) => return,
        };
        if let Some(id) = value.get("id") {
            if let Some(method) = value.get("method").and_then(Value::as_str) {
                // Incoming server request: answer with a harmless default so the
                // server does not stall waiting for us.
                let reply = respond_to_request(id, method);
                self.send_message(reply);
            } else if let Some(id_num) = id.as_u64() {
                let result = value
                    .get("result")
                    .cloned()
                    .unwrap_or_else(|| value.get("error").cloned().unwrap_or(Value::Null));
                if self.debug {
                    eprintln!(
                        "[lsp] resp id={id_num} {}",
                        serde_json::to_string(&result)
                            .unwrap_or_default()
                            .chars()
                            .take(160)
                            .collect::<String>()
                    );
                }
                if let Some(tx) = self.pending.lock().unwrap().remove(&id_num) {
                    let _ = tx.send(result);
                }
            }
        } else if let Some(m) = value.get("method").and_then(Value::as_str) {
            if m == "textDocument/publishDiagnostics" {
                let params = &value["params"];
                if self.debug {
                    let uri = params.get("uri").and_then(Value::as_str).unwrap_or("");
                    let n = params
                        .get("diagnostics")
                        .and_then(Value::as_array)
                        .map(Vec::len)
                        .unwrap_or(0);
                    eprintln!("[lsp] publish uri={uri} n={n}");
                }
                if let Some((key, diags)) = parse_publish_diagnostics(params) {
                    self.diag_gen
                        .lock()
                        .unwrap()
                        .entry(key.clone())
                        .and_modify(|g| *g += 1)
                        .or_insert(1);
                    self.diags.lock().unwrap().insert(key, diags);
                }
            } else if self.debug {
                eprintln!("[lsp] method {m}");
            }
        }
    }

    fn send_message(&self, msg: Value) {
        if self.debug {
            let m = msg
                .get("method")
                .and_then(Value::as_str)
                .unwrap_or("<response>");
            eprintln!("[lsp] send {m}");
        }
        let body = serde_json::to_vec(&msg).unwrap_or_default();
        let mut frame = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
        frame.extend_from_slice(&body);
        let tx = self.out_tx.lock().unwrap().clone();
        if let Some(tx) = tx {
            let _ = tx.send(frame);
        }
    }

    /// Stop the server: drop the writer channel (so its thread exits) and kill
    /// the child process. Idempotent and best-effort.
    fn shutdown(&self) {
        self.alive.store(false, Ordering::SeqCst);
        if let Ok(mut tx) = self.out_tx.lock() {
            *tx = None;
        }
        if let Ok(mut child) = self.child.lock() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    fn request(&self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.id_seq.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = mpsc::channel();
        {
            let mut pending = self.pending.lock().unwrap();
            pending.insert(id, tx);
        }
        self.send_message(
            json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }),
        );
        rx.recv_timeout(Duration::from_secs(20))
            .map_err(|_| format!("timed out waiting for `{}` response", method))
    }

    /// Ensure `uri`'s file is open with the given text; sends `didOpen` on
    /// first sight and updates the open document when the disk content moved on.
    /// Returns `true` when the server was asked to re-analyze.
    fn open_or_change(&self, uri: &str, language: &str, content: &str) -> bool {
        let mut docs = self.docs.lock().unwrap();
        match docs.get_mut(uri) {
            Some((version, last)) => {
                if last.as_deref() == Some(content) {
                    return false;
                }
                // A `contentChanges` entry with no `range` replaces the whole
                // document. That is legal under both full and incremental sync,
                // so the same call reaches every server without having to compute
                // an offset against text the server may already have moved past.
                *version += 1;
                let params = json!({
                    "textDocument": { "uri": uri, "version": *version },
                    "contentChanges": [{ "text": content }],
                });
                self.send_message(json!({ "jsonrpc": "2.0", "method": "textDocument/didChange", "params": params }));
                last.replace(content.to_string());
                return true;
            }
            None => {
                let params = json!({
                    "textDocument": {
                        "uri": uri,
                        "languageId": language,
                        "version": 1,
                        "text": content,
                    }
                });
                self.send_message(
                    json!({ "jsonrpc": "2.0", "method": "textDocument/didOpen", "params": params }),
                );
                docs.insert(uri.to_string(), (1, Some(content.to_string())));
                false
            }
        }
    }

    /// Poll for a fresh `publishDiagnostics` after an open/change, or reuse the
    /// last known set for the URI when the server says nothing new.
    /// `reanalysis` is set when the document was re-sent, where the first fresh
    /// publish can still be transient (a stale echo of the pre-edit state, or
    /// the empty set a close can produce): let the server settle and answer with
    /// the freshest set it published.
    fn wait_publish(&self, uri: &str, reanalysis: bool) -> Vec<LspDiagnostic> {
        let key = case_key(&uri_to_path(uri));
        let baseline = self
            .diag_gen
            .lock()
            .unwrap()
            .get(&key)
            .copied()
            .unwrap_or(0);
        // Poll up to ~5s: language servers usually publish findings 1-3s after
        // `didOpen` (analysis/rustc), which is longer under CPU contention.
        for _ in 0..100 {
            if !self.alive.load(Ordering::SeqCst) {
                break;
            }
            if self
                .diag_gen
                .lock()
                .unwrap()
                .get(&key)
                .copied()
                .unwrap_or(baseline)
                > baseline
            {
                if reanalysis {
                    std::thread::sleep(Duration::from_millis(400));
                    return self.cached_diags(&key);
                }
                let current = self.cached_diags(&key);
                if !current.is_empty() {
                    // Fresh findings right after `didOpen`; servers also echo an
                    // empty set before analysis lands, so only findings are
                    // taken as an early answer.
                    return current;
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        self.cached_diags(&key)
    }

    fn cached_diags(&self, key: &str) -> Vec<LspDiagnostic> {
        self.diags
            .lock()
            .unwrap()
            .get(key)
            .cloned()
            .unwrap_or_default()
    }

    /// Preferred route: poll `textDocument/diagnostic` a few times so servers
    /// that answer with an early "still loading" empty snapshot (rust-analyzer)
    /// get a chance to report findings once analysis completes; fall back to the
    /// publish cache for servers without pull support.
    fn diagnostics_for(&self, uri: &str, reanalysis: bool) -> Vec<LspDiagnostic> {
        if self.pull.load(Ordering::SeqCst) {
            let mut answered = false;
            for attempt in 0..3 {
                match self.request(
                    "textDocument/diagnostic",
                    json!({ "textDocument": { "uri": uri } }),
                ) {
                    Ok(result) => {
                        answered = true;
                        let items = pull_items(&result);
                        if !items.is_empty() {
                            let rel = uri_to_path(uri);
                            return items
                                .iter()
                                .filter_map(|d| canonicalize(uri, &rel, d))
                                .collect();
                        }
                    }
                    Err(_) => break,
                }
                if attempt < 2 {
                    std::thread::sleep(Duration::from_millis(800));
                }
            }
            if answered {
                // The server owns this document's diagnostics and reported a
                // (possibly clean) snapshot; no need for the publish cache.
                return Vec::new();
            }
        }
        self.wait_publish(uri, reanalysis)
    }
}

/// Answer an incoming server request so it never waits on us. LSP servers do
/// not depend on these, so empty/`null` results keep them moving.
fn respond_to_request(id: &Value, method: &str) -> Value {
    let result = match method {
        "workspace/configuration" => Value::Array(Vec::new()),
        "workspace/workspaceFolders" => Value::Array(Vec::new()),
        "workspace/applyEdit" => json!({ "applied": false }),
        _ => Value::Null,
    };
    json!({ "jsonrpc": "2.0", "id": id.clone(), "result": result })
}

/// `file:///…` URI for a path (spaces et al. percent-encoded by `url`).
fn file_uri(path: &Path) -> String {
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(path)
    };
    let display = abs.to_string_lossy().replace('\\', "/");
    url::Url::from_file_path(&abs)
        .map(|u| lower_drive(&u.to_string()))
        .unwrap_or_else(|_| format!("file://{display}"))
}

/// Normalize a `file://` URI to the form servers echo back: a lower-case drive
/// letter. Without this, `file:///D:/x.rs` (from an absolute Windows path) and
/// the server's `file:///d:/x.rs` are different documents, so an open-document
/// overlay is never applied to the file the server actually analyses.
fn lower_drive(uri: &str) -> String {
    #[cfg(windows)]
    {
        // "file:///" is 8 bytes, so the drive letter sits at index 8.
        if uri.len() > 9 && uri.starts_with("file:///") {
            let drive = uri.as_bytes()[8];
            if drive.is_ascii_uppercase() {
                let mut out = uri.as_bytes().to_vec();
                out[8] = drive.to_ascii_lowercase();
                return String::from_utf8(out).unwrap_or_else(|_| uri.to_string());
            }
        }
    }
    uri.to_string()
}

/// Percent-decode a `file://` URI back to a plain path string.
fn uri_to_path(uri: &str) -> String {
    uri.strip_prefix("file://")
        .map(|rest| {
            let decoded = percent_decode(rest);
            #[cfg(windows)]
            let decoded = decoded.strip_prefix('/').unwrap_or(&decoded).to_string();
            decoded.replace('\\', "/")
        })
        .unwrap_or_else(|| uri.to_string())
}

/// Storage key for diagnostics maps: case-folded on Windows so the URI casing
/// the server publishes (e.g. `file:///d:/…`) matches the casing we derive from
/// the on-disk path (e.g. `D:\…`).
fn case_key(path: &str) -> String {
    #[cfg(windows)]
    {
        path.to_lowercase()
    }
    #[cfg(not(windows))]
    {
        path.to_string()
    }
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3])
                .ok()
                .and_then(|h| u8::from_str_radix(h, 16).ok());
            if let Some(b) = hex {
                out.push(b);
                i += 3;
                continue;
            }
        }
        if bytes[i] == b'+' {
            // LSP file URIs do not URL-encode spaces as `+`, but be lenient.
            out.push(b' ');
            i += 1;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

impl Drop for LspSession {
    fn drop(&mut self) {
        if let Ok(mut tx) = self.out_tx.lock() {
            *tx = None;
        }
        if let Ok(mut child) = self.child.lock() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Per-language server sessions, cached by (server key, project root).
pub struct LspManager {
    sessions: Mutex<HashMap<(String, PathBuf), Arc<LspSession>>>,
}

impl Default for LspManager {
    fn default() -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
        }
    }
}

impl LspManager {
    /// Resolve + spawn the server for `path`, open the file, and return its
    /// diagnostics. Never errors on absent servers: it reports `server: None`
    /// and the caller shows the hint instead.
    pub fn diagnostics(&self, app: &AppHandle, path: &str) -> LspDiagnostics {
        let pathbuf = PathBuf::from(path);
        let Some(language) = language_for(&pathbuf) else {
            return LspDiagnostics {
                server: None,
                diagnostics: Vec::new(),
            };
        };
        let Some(spec) = spec_for(app, language) else {
            return LspDiagnostics {
                server: None,
                diagnostics: Vec::new(),
            };
        };
        self.diagnostics_with(&pathbuf, language, &spec)
    }

    /// Same as [`Self::diagnostics`] with the builtin server table only (no
    /// `lspServers` settings overrides) — used by the post-edit verify pass,
    /// which runs outside the command layer and has no [`AppHandle`].
    pub fn diagnostics_path(&self, path: &std::path::Path) -> Option<LspDiagnostics> {
        let language = language_for(path)?;
        let spec = builtin_spec(language)?;
        Some(self.diagnostics_with(path, language, &spec))
    }

    /// Same as [`Self::diagnostics`] with the spec already resolved (used by
    /// the verify pass and live tests that build specs directly).
    pub fn diagnostics_with(
        &self,
        path: &Path,
        language: &str,
        spec: &LspServerSpec,
    ) -> LspDiagnostics {
        let root = resolve_root(path, language);
        let key = (spec.key.clone(), root.clone());
        let session = {
            let mut sessions = self.sessions.lock().unwrap();
            if let Some(s) = sessions.get(&key) {
                Arc::clone(s)
            } else {
                let s = match LspSession::spawn(spec, &root, path) {
                    Ok(s) => s,
                    Err(_) => {
                        return LspDiagnostics {
                            server: Some(spec.display.clone()),
                            diagnostics: Vec::new(),
                        }
                    }
                };
                sessions.insert(key, Arc::clone(&s));
                s
            }
        };
        let uri = file_uri(path);
        let content = std::fs::read_to_string(path).unwrap_or_default();
        let reanalyzed = session.open_or_change(&uri, language, &content);
        let diagnostics = session.diagnostics_for(&uri, reanalyzed);
        if session.debug {
            eprintln!(
                "[lsp] diagnostics_with uri={uri} n={} server={}",
                diagnostics.len(),
                spec.display
            );
        }
        LspDiagnostics {
            server: Some(spec.display.clone()),
            diagnostics,
        }
    }

    /// Kill every cached server and clear the cache. Called on workspace change
    /// and app exit so language servers never outlive the workspace they served.
    pub fn shutdown_all(&self) {
        let sessions: Vec<Arc<LspSession>> = {
            let mut map = self.sessions.lock().unwrap();
            map.drain().map(|(_, s)| s).collect()
        };
        for s in sessions {
            s.shutdown();
        }
    }
}

/// tauri-managed handle passed around (editor gutter + verify pass).
pub type SharedLspManager = Arc<LspManager>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_for_detects_supported_files() {
        assert_eq!(language_for(Path::new("src/main.rs")), Some("rust"));
        assert_eq!(
            language_for(Path::new("ui/src/App.tsx")),
            Some("typescript")
        );
        assert_eq!(language_for(Path::new("app.py")), Some("python"));
        assert_eq!(language_for(Path::new("README.md")), None);
        assert_eq!(language_for(Path::new("Makefile")), None);
    }

    #[test]
    fn resolve_root_finds_project_markers() {
        let root = std::env::temp_dir().join("lsp-root-test");
        let pkg = root.join("pkg").join("Cargo.toml");
        let file = root.join("pkg").join("src").join("main.rs");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&pkg, "[package]").unwrap();
        // Cleanup in reverse so dirs unwinds.
        let hit = resolve_root(&file, "rust");
        assert_eq!(hit, root.join("pkg"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn resolve_root_falls_back_to_file_dir() {
        let dir = std::env::temp_dir().join(format!("lsp-fallback-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("orphan.rs");
        std::fs::write(&file, "fn main() {}").unwrap();
        assert_eq!(resolve_root(&file, "rust"), dir);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn framing_parses_content_length() {
        let mut buf = b"Content-Length: 13\r\n\r\n{\"a\":\"bcdef\"}junk".to_vec();
        let body = try_take_message(&mut buf).expect("frame parsed");
        assert_eq!(&body[..], b"{\"a\":\"bcdef\"}");
        assert_eq!(buf, b"junk");
    }

    #[test]
    fn framing_waits_for_incomplete_body() {
        let mut buf = b"Content-Length: 100\r\n\r\nshort".to_vec();
        assert!(try_take_message(&mut buf).is_none());
        assert_eq!(buf.len(), "Content-Length: 100\r\n\r\nshort".len());
    }

    #[test]
    fn canonicalize_maps_lsp_positions_and_severity() {
        let uri = "file:///d%3A/some/file.rs";
        let value = json!({
            "range": { "start": { "line": 3, "character": 0 }, "end": { "line": 3, "character": 5 } },
            "severity": 2,
            "code": "E0502",
            "message": "cannot borrow `x` as mutable"
        });
        let d = canonicalize(uri, "d:/some/file.rs", &value).expect("diag");
        assert_eq!(d.line, 4, "0-based LSP → 1-based display");
        assert_eq!(d.column, 1);
        assert_eq!(d.severity, "warning");
        assert_eq!(d.code.as_deref(), Some("E0502"));
        assert!(d.message.contains("borrow"));
    }

    #[test]
    fn canonicalize_defaults_severity_to_error() {
        let value = json!({
            "range": { "start": { "line": 0, "character": 0 } },
            "message": "syntax error"
        });
        let d = canonicalize("file:///x.py", "x.py", &value).expect("diag");
        assert_eq!(d.severity, "error");
        assert_eq!(d.code, None);
    }

    #[test]
    fn unknown_language_reports_no_server() {
        assert_eq!(language_for(Path::new("README.md")), None);
    }

    #[test]
    fn publish_diagnostics_bridge_maps_a_fake_jsonrpc_notification() {
        // A `textDocument/publishDiagnostics` payload exactly as a server emits it.
        let params = json!({
            "uri": "file:///d%3A/proj/src/lib.rs",
            "diagnostics": [
                {
                    "range": { "start": { "line": 9, "character": 4 }, "end": { "line": 9, "character": 9 } },
                    "severity": 1,
                    "code": "E0308",
                    "message": "mismatched types"
                },
                {
                    "range": { "start": { "line": 1, "character": 0 } },
                    "severity": 2,
                    "message": "unused variable"
                }
            ]
        });

        let (key, diags) = parse_publish_diagnostics(&params).expect("parsed");
        assert_eq!(key, "d:/proj/src/lib.rs", "path is case-keyed + decoded");
        assert_eq!(diags.len(), 2);
        assert_eq!(diags[0].line, 10, "0-based → 1-based");
        assert_eq!(diags[0].column, 5);
        assert_eq!(diags[0].severity, "error");
        assert_eq!(diags[0].code.as_deref(), Some("E0308"));
        assert_eq!(diags[1].severity, "warning");
        assert_eq!(diags[1].path, "d:/proj/src/lib.rs");
    }

    #[test]
    fn publish_diagnostics_tolerates_empty_and_missing_uri() {
        // No URI → no key (nothing to store).
        assert!(parse_publish_diagnostics(&json!({ "diagnostics": [] })).is_none());

        // URI but no diagnostics array → an empty set (clears prior findings).
        let (key, diags) = parse_publish_diagnostics(&json!({ "uri": "file:///x.py" })).unwrap();
        assert_eq!(key, "x.py");
        assert!(diags.is_empty());
    }

    #[test]
    fn spec_for_applies_settings_overrides() {
        let over = json!({
            "rust": { "enabled": false },
            "python": { "command": "basedpyright-langserver", "args": ["--stdio", "-v"], "pull": false }
        });
        assert!(
            spec_with_extra("rust", Some(&over)).is_none(),
            "disabled server is dropped"
        );
        let py = spec_with_extra("python", Some(&over)).expect("python spec");
        assert_eq!(py.command, "basedpyright-langserver");
        assert_eq!(py.args, vec!["--stdio", "-v"]);
        assert!(!py.pull);
        // Untouched language keeps its builtin defaults.
        let ts = spec_with_extra("typescript", Some(&over)).expect("ts spec");
        assert_eq!(ts.command, "typescript-language-server");
        assert!(!ts.pull);
    }

    #[test]
    fn spec_for_falls_back_to_builtin_without_settings() {
        assert_eq!(
            spec_with_extra("rust", None).expect("rust").command,
            "rust-analyzer"
        );
        assert!(spec_with_extra("rust", Some(&json!({}))).is_some());
        assert!(spec_with_extra("unknown", None).is_none());
    }

    /// Live tests spawn a real language server and wait on its analysis, so they
    /// must not overlap: two rust-analyzers analysing throwaway projects at once
    /// push the first publish past the poll budget and the run gets flaky.
    static LIVE_LSP: Mutex<()> = Mutex::new(());

    /// Re-query diagnostics until they reach `want_error` (or the budget runs
    /// out) and return the last set. One `diagnostics_with` call waits ~5s for a
    /// publish, which a fully loaded test binary can outrun, so the live tests
    /// assert eventual consistency rather than a single-shot answer.
    fn poll_diagnostics(
        mgr: &LspManager,
        file: &Path,
        spec: &LspServerSpec,
        want_error: bool,
    ) -> Vec<LspDiagnostic> {
        let mut last = Vec::new();
        for _ in 0..20 {
            last = mgr.diagnostics_with(file, "rust", spec).diagnostics;
            if last.iter().any(|d| d.severity == "error") == want_error {
                break;
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        last
    }

    /// End-to-end against the real `rust-analyzer` (when installed): a file
    /// with a syntax error must come back with at least one diagnostic. Skipped
    /// silently on machines without the binary so CI stays green.
    #[test]
    fn live_rust_analyzer_reports_syntax_errors() {
        if std::process::Command::new("rust-analyzer")
            .arg("--version")
            .output()
            .is_err()
        {
            return;
        }
        let _serial = LIVE_LSP.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("lsp-live-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(
            dir.join("Cargo.toml"),
            "[package]\nname = \"lsprobe\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        let file = dir.join("src").join("broken.rs");
        std::fs::write(
            &file,
            "fn main() {\n    let x = ;\n    println!(\"{x}\");\n}\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("src").join("main.rs"),
            "mod broken;\nfn main() {}\n",
        )
        .unwrap();

        let mgr = LspManager::default();
        let spec = builtin_spec("rust").expect("rust spec");
        let res = mgr.diagnostics_with(&file, "rust", &spec);
        assert_eq!(
            res.server.as_deref(),
            Some("rust-analyzer"),
            "server reported"
        );
        let diags = poll_diagnostics(&mgr, &file, &spec, true);
        assert!(
            diags.iter().any(|d| d.severity == "error"),
            "rust-analyzer should flag `let x = ;` as an error (got {diags:#?})",
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Phase-2 acceptance, end to end against the real `rust-analyzer`: a
    /// syntax error is reported, the agent's fix lands on disk (a re-analysis
    /// of the already-open document), and the next diagnostics call comes back
    /// clean — the "diagnostics show → agent fixes → diagnostics clear" loop.
    /// Skipped silently when the binary is missing.
    #[test]
    fn live_diagnostics_clear_after_the_fix_is_applied() {
        if std::process::Command::new("rust-analyzer")
            .arg("--version")
            .output()
            .is_err()
        {
            return;
        }
        let _serial = LIVE_LSP.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("lsp-accept-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(
            dir.join("Cargo.toml"),
            "[package]\nname = \"acceptprobe\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        std::fs::write(dir.join("src").join("main.rs"), "mod calc;\nfn main() {}\n").unwrap();
        let file = dir.join("src").join("calc.rs");
        std::fs::write(&file, "pub fn add(a: i32, b: i32) -> i32 {\n    a +\n}\n").unwrap();

        let mgr = LspManager::default();
        let spec = builtin_spec("rust").expect("rust spec");
        let broken = mgr.diagnostics_with(&file, "rust", &spec);
        assert_eq!(broken.server.as_deref(), Some("rust-analyzer"));
        let broken_diags = poll_diagnostics(&mgr, &file, &spec, true);
        assert!(
            broken_diags.iter().any(|d| d.severity == "error"),
            "step 1: the broken file must report an error, got {broken_diags:#?}"
        );

        // Step 2: the agent's edit is applied to disk.
        std::fs::write(&file, "pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n").unwrap();

        let fixed = poll_diagnostics(&mgr, &file, &spec, false);
        assert!(
            !fixed.iter().any(|d| d.severity == "error"),
            "step 2: diagnostics must clear after the fix is applied, got {fixed:#?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
