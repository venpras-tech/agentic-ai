//! Minimal MCP (Model Context Protocol) client.
//!
//! Speaks JSON-RPC 2.0 either over a spawned server's stdin/stdout (stdio
//! transport) or over HTTP (Streamable HTTP transport, incl. Server-Sent
//! Events responses). Each connection performs the `initialize` handshake once,
//! then serves `tools/call` requests. This lets the orchestrator extend itself
//! with any MCP server the user installs or hosts (playwright, puppeteer,
//! filesystem, postgres, …) without any code changes.
//!
//! A persisted *catalog* (`{config}/mcp-servers.json`) holds named server
//! configs so the model can call them by name and the user can manage them
//! from the settings UI without re-entering command lines every session.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

/// How to reach an MCP server: spawn a local process, or call a remote URL.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Transport {
    /// Spawn `bin args…` and speak JSON-RPC over its stdin/stdout.
    #[default]
    Stdio,
    /// POST JSON-RPC to `url` (Streamable HTTP; JSON or SSE responses).
    Http,
}

/// One configured MCP server in the user's catalog.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServerConfig {
    /// Short unique identifier used in tool calls (`"server": "playwright"`).
    pub name: String,
    /// Transport selector; defaults to stdio when absent.
    #[serde(default)]
    pub transport: Transport,
    /// Executable (PATH lookup or absolute path) for the stdio transport.
    #[serde(default)]
    pub bin: String,
    /// Command-line arguments passed to the executable.
    #[serde(default)]
    pub args: Vec<String>,
    /// Extra environment variables for the spawned process.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Endpoint URL for the HTTP transport (e.g. `https://host/mcp`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Extra request headers for the HTTP transport (e.g. Authorization).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
    /// When non-empty, only these tool names are callable through this
    /// server (a trailing `*` acts as a prefix wildcard).
    #[serde(default)]
    pub allowed_tools: Vec<String>,
    /// Disabled entries stay in the catalog but are not callable.
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}

impl McpServerConfig {
    /// Whether `tool` may be called through this server. An empty allow-list
    /// means everything is allowed; a trailing `*` in an entry acts as a
    /// prefix wildcard (`"mcp__playwright__*"` style).
    #[allow(dead_code)]
    pub fn tool_allowed(&self, tool: &str) -> bool {
        Self::matches_allow_list(&self.allowed_tools, tool)
    }

    /// Allow-list matcher shared with ad-hoc calls (empty list = allow all).
    #[allow(dead_code)]
    pub fn matches_allow_list(list: &[String], tool: &str) -> bool {
        if list.is_empty() {
            return true;
        }
        list.iter().any(|entry| match entry.strip_suffix('*') {
            Some(prefix) => tool.starts_with(prefix),
            None => entry == tool,
        })
    }

    /// Stable cache key for a live connection (transport + target).
    pub fn cache_key(&self) -> String {
        match self.transport {
            Transport::Http => format!("http {}", self.url.clone().unwrap_or_default()),
            Transport::Stdio => format!("stdio {} {}", self.bin, self.args.join(" ")),
        }
    }

    /// Whether the config has enough fields to attempt a connection.
    pub fn validate(&self) -> Result<(), String> {
        match self.transport {
            Transport::Stdio if self.bin.trim().is_empty() => {
                Err(format!("Server `{}` needs a non-empty command", self.name))
            }
            Transport::Http => match self.url.as_deref() {
                Some(u) if !u.trim().is_empty() => Ok(()),
                _ => Err(format!(
                    "Server `{}` needs a URL for the http transport",
                    self.name
                )),
            },
            _ => Ok(()),
        }
    }
}

/// Path of the persisted catalog inside the app config dir.
pub fn catalog_path(config_dir: &Path) -> PathBuf {
    config_dir.join("mcp-servers.json")
}

/// Load the user's MCP server catalog. A missing file means "empty catalog",
/// not an error; malformed JSON surfaces as a hard error.
pub fn load_catalog(config_dir: &Path) -> Result<Vec<McpServerConfig>, String> {
    let path = catalog_path(config_dir);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text =
        std::fs::read_to_string(&path).map_err(|e| format!("Failed to read catalog: {e}"))?;
    serde_json::from_str(&text).map_err(|e| format!("Catalog `{}` is invalid: {e}", path.display()))
}

/// Persist the catalog (pretty-printed for easy hand editing).
pub fn save_catalog(config_dir: &Path, servers: &[McpServerConfig]) -> Result<(), String> {
    let path = catalog_path(config_dir);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir failed: {e}"))?;
    }
    let json = serde_json::to_string_pretty(servers).map_err(|e| e.to_string())?;
    std::fs::write(&path, json).map_err(|e| format!("Failed to write catalog: {e}"))
}

/// A live connection to a stdio MCP server. Not `Sync`; wrap in a mutex.
pub struct McpHandle {
    stdin: tokio::process::ChildStdin,
    stdout: BufReader<tokio::process::ChildStdout>,
    next_id: u32,
}

impl McpHandle {
    /// Spawn `bin args…` with `env` layered on top of the inherited
    /// environment, then complete the JSON-RPC `initialize` handshake.
    pub async fn spawn(
        bin: &str,
        args: &[String],
        env: &BTreeMap<String, String>,
    ) -> Result<McpHandle, String> {
        let mut cmd = tokio::process::Command::new(bin);
        cmd.args(args)
            .envs(env)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::inherit())
            .kill_on_drop(true);
        #[cfg(windows)]
        {
            cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("Failed to spawn MCP server `{bin}`: {e}"))?;
        let stdin = child
            .stdin
            .take()
            .ok_or("MCP server did not expose stdin")?;
        let stdout = child
            .stdout
            .take()
            .ok_or("MCP server did not expose stdout")?;
        let mut handle = McpHandle {
            stdin,
            stdout: BufReader::new(stdout),
            next_id: 1,
        };

        handle
            .request(
                "initialize",
                json!({
                    "protocolVersion": "2024-11-05",
                    "capabilities": {},
                    "clientInfo": { "name": "ai-editor-agent", "version": "0.1.0" }
                }),
            )
            .await?;
        handle
            .notify("notifications/initialized", json!({}))
            .await?;
        Ok(handle)
    }

    /// Perform an MCP tool call and return the structured content.
    pub async fn call_tool(&mut self, tool: &str, arguments: &Value) -> Result<Value, String> {
        let params = json!({
            "name": tool,
            "arguments": arguments
        });
        let resp = self.request("tools/call", params).await?;
        extract_call_result(&resp)
    }

    /// Ask the server which tools it exposes via `tools/list`.
    pub async fn list_tools(&mut self) -> Result<Vec<String>, String> {
        let resp = self.request("tools/list", json!({})).await?;
        Ok(parse_tools(&resp))
    }

    /// Ask the server which tools it exposes via `tools/list`, returning full
    /// tool definitions (name, description, inputSchema).
    pub async fn list_tool_defs(&mut self) -> Result<Vec<Value>, String> {
        let resp = self.request("tools/list", json!({})).await?;
        Ok(parse_tool_defs(&resp))
    }

    async fn request(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next_id;
        self.next_id += 1;
        self.write_frame(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        }))
        .await?;
        loop {
            let raw = self.read_frame().await?;
            let msg: Value =
                serde_json::from_str(&raw).map_err(|e| format!("Bad JSON-RPC message: {e}"))?;
            if msg.get("id").and_then(|i| i.as_u64()) != Some(id as u64) {
                continue; // ignore notifications/other responses
            }
            if msg.get("error").is_some() {
                return Err(msg.get("error").unwrap().to_string());
            }
            return Ok(msg);
        }
    }

    async fn notify(&mut self, method: &str, params: Value) -> Result<(), String> {
        self.write_frame(&json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params
        }))
        .await
    }

    async fn write_frame(&mut self, value: &Value) -> Result<(), String> {
        use tokio::io::AsyncWriteExt;
        let bytes = encode_frame(value)?;
        self.stdin
            .write_all(&bytes)
            .await
            .map_err(|e| e.to_string())?;
        self.stdin.flush().await.map_err(|e| e.to_string())
    }

    async fn read_frame(&mut self) -> Result<String, String> {
        read_frame_from(&mut self.stdout).await
    }
}

/// Encode a JSON-RPC message using the MCP stdio transport framing
/// (`Content-Length: N\r\n\r\nBODY`). Pure, so it is unit-testable.
fn encode_frame(value: &Value) -> Result<Vec<u8>, String> {
    let body = serde_json::to_vec(value).map_err(|e| e.to_string())?;
    let header = format!("Content-Length: {}\r\n\r\n", body.len());
    let mut out = Vec::with_capacity(header.len() + body.len());
    out.extend_from_slice(header.as_bytes());
    out.extend_from_slice(&body);
    Ok(out)
}

/// Read one Content-Length framed JSON-RPC message from any buffered async
/// reader. Extracted from `McpHandle` so the framing can be tested over an
/// in-memory duplex without spawning a child process.
async fn read_frame_from<R>(reader: &mut R) -> Result<String, String>
where
    R: tokio::io::AsyncBufRead + Unpin,
{
    use tokio::io::AsyncReadExt;
    let mut content_length: Option<usize> = None;
    loop {
        let mut line = String::new();
        reader
            .read_line(&mut line)
            .await
            .map_err(|e| e.to_string())?;
        if line.is_empty() {
            return Err("MCP server closed the connection".to_string());
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break;
        }
        if let Some(v) = trimmed.strip_prefix("Content-Length:") {
            content_length = Some(
                v.trim()
                    .parse()
                    .map_err(|_| "Bad Content-Length header".to_string())?,
            );
        }
    }
    let len = content_length.ok_or("MCP frame missing Content-Length header")?;
    if len > 64 * 1024 * 1024 {
        return Err("MCP frame too large".to_string());
    }
    let mut body = vec![0u8; len];
    reader
        .read_exact(&mut body)
        .await
        .map_err(|e| e.to_string())?;
    String::from_utf8(body).map_err(|e| e.to_string())
}

/// A live MCP connection that may be either transport. Call sites use this and
/// stay agnostic about stdio vs HTTP.
pub enum McpClient {
    Stdio(McpHandle),
    Http(HttpClient),
}

impl McpClient {
    /// Connect to a configured server, completing the `initialize` handshake.
    pub async fn connect(cfg: &McpServerConfig) -> Result<McpClient, String> {
        cfg.validate()?;
        match cfg.transport {
            Transport::Stdio => Ok(McpClient::Stdio(
                McpHandle::spawn(&cfg.bin, &cfg.args, &cfg.env).await?,
            )),
            Transport::Http => Ok(McpClient::Http(HttpClient::connect(cfg).await?)),
        }
    }

    /// Perform an MCP tool call and return the structured content.
    pub async fn call_tool(&mut self, tool: &str, arguments: &Value) -> Result<Value, String> {
        match self {
            McpClient::Stdio(h) => h.call_tool(tool, arguments).await,
            McpClient::Http(h) => h.call_tool(tool, arguments).await,
        }
    }

    /// Ask the server which tools it exposes via `tools/list`.
    pub async fn list_tools(&mut self) -> Result<Vec<String>, String> {
        match self {
            McpClient::Stdio(h) => h.list_tools().await,
            McpClient::Http(h) => h.list_tools().await,
        }
    }

    /// Ask the server which tools it exposes via `tools/list`, returning full definitions.
    pub async fn list_tool_defs(&mut self) -> Result<Vec<Value>, String> {
        match self {
            McpClient::Stdio(h) => h.list_tool_defs().await,
            McpClient::Http(h) => h.list_tool_defs().await,
        }
    }
}

/// JSON-RPC over HTTP (MCP "Streamable HTTP" transport). A response may arrive
/// as a plain JSON body or as a short Server-Sent Events stream; when the server
/// issues an `Mcp-Session-Id` header we echo it on every subsequent request.
pub struct HttpClient {
    client: reqwest::Client,
    url: String,
    headers: BTreeMap<String, String>,
    session_id: Option<String>,
    next_id: u32,
}

impl HttpClient {
    async fn connect(cfg: &McpServerConfig) -> Result<HttpClient, String> {
        let url = cfg
            .url
            .clone()
            .ok_or_else(|| "MCP http server missing `url`".to_string())?;
        let client = reqwest::Client::builder()
            .build()
            .map_err(|e| format!("Failed to build HTTP client: {e}"))?;
        let mut handle = HttpClient {
            client,
            url,
            headers: cfg.headers.clone(),
            session_id: None,
            next_id: 1,
        };
        handle
            .request(
                "initialize",
                json!({
                    "protocolVersion": "2024-11-05",
                    "capabilities": {},
                    "clientInfo": { "name": "ai-editor-agent", "version": "0.1.0" }
                }),
            )
            .await?;
        handle
            .notify("notifications/initialized", json!({}))
            .await?;
        Ok(handle)
    }

    pub async fn call_tool(&mut self, tool: &str, arguments: &Value) -> Result<Value, String> {
        let params = json!({ "name": tool, "arguments": arguments });
        let resp = self.request("tools/call", params).await?;
        extract_call_result(&resp)
    }

    pub async fn list_tools(&mut self) -> Result<Vec<String>, String> {
        let resp = self.request("tools/list", json!({})).await?;
        Ok(parse_tools(&resp))
    }

    pub async fn list_tool_defs(&mut self) -> Result<Vec<Value>, String> {
        let resp = self.request("tools/list", json!({})).await?;
        Ok(parse_tool_defs(&resp))
    }

    fn apply_headers(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        let mut req = req
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream");
        for (k, v) in &self.headers {
            req = req.header(k, v);
        }
        if let Some(sid) = &self.session_id {
            req = req.header("Mcp-Session-Id", sid);
        }
        req
    }

    async fn request(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next_id;
        self.next_id += 1;
        let body = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        });
        let resp = self
            .apply_headers(self.client.post(&self.url))
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("MCP HTTP request failed: {e}"))?;
        self.capture_session(&resp);
        let status = resp.status();
        if !status.is_success() {
            return Err(format!("MCP HTTP request failed: {status}"));
        }
        let is_sse = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .map(|c| c.contains("text/event-stream"))
            .unwrap_or(false);
        if is_sse {
            self.read_sse_response(resp, id).await
        } else {
            let msg: Value = resp
                .json()
                .await
                .map_err(|e| format!("Bad MCP JSON response: {e}"))?;
            if msg.get("id").and_then(|i| i.as_u64()) != Some(id as u64) {
                return Err("MCP response id mismatch".to_string());
            }
            unwrap_response(&msg)
        }
    }

    async fn notify(&mut self, method: &str, params: Value) -> Result<(), String> {
        let body = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params
        });
        let resp = self
            .apply_headers(self.client.post(&self.url))
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("MCP HTTP notify failed: {e}"))?;
        self.capture_session(&resp);
        Ok(())
    }

    fn capture_session(&mut self, resp: &reqwest::Response) {
        if let Some(sid) = resp
            .headers()
            .get("mcp-session-id")
            .and_then(|v| v.to_str().ok())
        {
            self.session_id = Some(sid.to_string());
        }
    }

    async fn read_sse_response(
        &mut self,
        resp: reqwest::Response,
        id: u32,
    ) -> Result<Value, String> {
        let mut stream = resp.bytes_stream();
        let mut buf = String::new();
        while let Some(chunk) = stream.next().await {
            let bytes = chunk.map_err(|e| format!("MCP SSE read failed: {e}"))?;
            // SSE line terminators are \r\n or \n; dropping \r keeps the parser
            // single-delimiter while JSON escapes any literal carriage returns.
            buf.push_str(&String::from_utf8_lossy(&bytes).replace('\r', ""));
            for payload in drain_sse_events(&mut buf) {
                let Ok(msg) = serde_json::from_str::<Value>(&payload) else {
                    continue;
                };
                if msg.get("id").and_then(|i| i.as_u64()) == Some(id as u64) {
                    return unwrap_response(&msg);
                }
            }
        }
        Err("MCP SSE stream ended before a response arrived".to_string())
    }
}

/// Pull every complete SSE event's concatenated `data:` payload out of `buf`,
/// removing the consumed bytes so a caller can keep appending chunks.
fn drain_sse_events(buf: &mut String) -> Vec<String> {
    let mut events = Vec::new();
    while let Some(idx) = buf.find("\n\n") {
        let block = buf[..idx].to_string();
        buf.drain(..idx + 2);
        let mut data = String::new();
        for line in block.lines() {
            if let Some(rest) = line.strip_prefix("data:") {
                let rest = rest.strip_prefix(' ').unwrap_or(rest);
                if !data.is_empty() {
                    data.push('\n');
                }
                data.push_str(rest);
            }
        }
        if !data.is_empty() {
            events.push(data);
        }
    }
    events
}

/// Validate a JSON-RPC response envelope, surfacing a JSON-RPC error if present.
fn unwrap_response(msg: &Value) -> Result<Value, String> {
    if let Some(err) = msg.get("error") {
        return Err(err.to_string());
    }
    Ok(msg.clone())
}

/// Extract the `{ text }` payload from a `tools/call` response.
fn extract_call_result(resp: &Value) -> Result<Value, String> {
    let err = resp
        .get("result")
        .and_then(|r| r.get("isError"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let content = resp
        .get("result")
        .and_then(|r| r.get("content"))
        .cloned()
        .unwrap_or_else(|| json!([]));
    let text = extract_text(&content);
    if err {
        if !text.trim().is_empty() {
            return Err(text);
        }
        if let Some(mcp_err) = resp.get("error").map(|e| e.to_string()) {
            return Err(mcp_err);
        }
        return Err("MCP tool reported an error".to_string());
    }
    Ok(json!({ "text": text }))
}

/// Collect + sort the tool names from a `tools/list` response.
fn parse_tools(resp: &Value) -> Vec<String> {
    let mut tools = Vec::new();
    if let Some(arr) = resp.pointer("/result/tools").and_then(|t| t.as_array()) {
        for t in arr {
            if let Some(name) = t.get("name").and_then(|n| n.as_str()) {
                tools.push(name.to_string());
            }
        }
    }
    tools.sort();
    tools
}

/// Return full tool definitions from a `tools/list` response.
fn parse_tool_defs(resp: &Value) -> Vec<Value> {
    if let Some(arr) = resp.pointer("/result/tools").and_then(|t| t.as_array()) {
        arr.iter().cloned().collect()
    } else {
        Vec::new()
    }
}

/// Flatten MCP `content` (list of `{type:"text"|"image"|…}` items) to a string.
fn extract_text(content: &Value) -> String {
    let mut parts = Vec::new();
    if let Some(items) = content.as_array() {
        for item in items {
            if let Some(text) = item.get("text").and_then(|t| t.as_str()) {
                parts.push(text.to_string());
            } else {
                parts.push(item.to_string());
            }
        }
    } else {
        parts.push(content.to_string());
    }
    parts.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_roundtrips_and_tolerates_missing_file() {
        let dir = std::env::temp_dir().join(format!("ai-mcp-cat-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();

        // Missing file -> empty catalog.
        assert!(load_catalog(&dir).unwrap().is_empty());

        let servers = vec![
            McpServerConfig {
                name: "playwright".into(),
                bin: "npx".into(),
                args: vec!["@playwright/mcp@latest".into()],
                env: BTreeMap::new(),
                allowed_tools: Vec::new(),
                enabled: true,
                ..Default::default()
            },
            McpServerConfig {
                name: "fs".into(),
                bin: "mcp-server-filesystem".into(),
                args: vec!["D:\\data".into()],
                env: BTreeMap::new(),
                allowed_tools: Vec::new(),
                enabled: false,
                ..Default::default()
            },
        ];
        save_catalog(&dir, &servers).unwrap();

        let loaded = load_catalog(&dir).unwrap();
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].name, "playwright");
        assert_eq!(loaded[0].args, vec!["@playwright/mcp@latest"]);
        assert!(loaded[0].enabled);
        assert!(!loaded[1].enabled); // explicit false survives

        // Default `enabled` is true when the field is absent.
        let bare: McpServerConfig =
            serde_json::from_str(r#"{"name":"x","bin":"x.exe","args":[]}"#).unwrap();
        assert!(bare.enabled);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn allowed_tools_filter_supports_wildcards() {
        let cfg = McpServerConfig {
            name: "playwright".into(),
            bin: "npx".into(),
            args: vec![],
            env: BTreeMap::new(),
            allowed_tools: vec!["browser_navigate".into(), "screenshot_*".into()],
            enabled: true,
            ..Default::default()
        };
        assert!(cfg.tool_allowed("browser_navigate"));
        assert!(cfg.tool_allowed("screenshot_full"));
        assert!(!cfg.tool_allowed("browser_click"));

        // Empty list = everything allowed; exact entries don't wildcard.
        let open = McpServerConfig {
            name: "fs".into(),
            bin: "x".into(),
            args: vec![],
            env: BTreeMap::new(),
            allowed_tools: Vec::new(),
            enabled: true,
            ..Default::default()
        };
        assert!(open.tool_allowed("anything"));

        let strict = McpServerConfig {
            name: "fs".into(),
            bin: "x".into(),
            args: vec![],
            env: BTreeMap::new(),
            allowed_tools: vec!["read_file".into()],
            enabled: true,
            ..Default::default()
        };
        assert!(strict.tool_allowed("read_file"));
        assert!(!strict.tool_allowed("read_files"));
    }

    #[test]
    fn env_and_allowed_tools_roundtrip_through_json() {
        let cfg: McpServerConfig = serde_json::from_str(
            r#"{"name":"db","bin":"db-mcp","args":[],"env":{"DB_URL":"x"},"allowedTools":["query*"]}"#,
        )
        .unwrap();
        assert_eq!(cfg.env.get("DB_URL").map(String::as_str), Some("x"));
        assert!(cfg.tool_allowed("query_rows"));
        assert!(!cfg.tool_allowed("drop_table"));
    }

    #[test]
    fn transport_defaults_to_stdio_and_http_roundtrips() {
        // Absent transport -> stdio (backwards compatible with old catalogs).
        let legacy: McpServerConfig =
            serde_json::from_str(r#"{"name":"x","bin":"x.exe","args":[]}"#).unwrap();
        assert_eq!(legacy.transport, Transport::Stdio);

        let remote: McpServerConfig = serde_json::from_str(
            r#"{"name":"remote","transport":"http","url":"https://host/mcp","headers":{"Authorization":"Bearer t"},"allowedTools":[]}"#,
        )
        .unwrap();
        assert_eq!(remote.transport, Transport::Http);
        assert_eq!(remote.url.as_deref(), Some("https://host/mcp"));
        assert_eq!(
            remote.headers.get("Authorization").map(String::as_str),
            Some("Bearer t")
        );
        assert!(remote.validate().is_ok());
    }

    #[test]
    fn http_config_requires_a_url_but_stdio_requires_a_command() {
        let no_url = McpServerConfig {
            name: "remote".into(),
            transport: Transport::Http,
            ..Default::default()
        };
        assert!(no_url.validate().is_err());

        let no_bin = McpServerConfig {
            name: "local".into(),
            transport: Transport::Stdio,
            ..Default::default()
        };
        assert!(no_bin.validate().is_err());
    }

    #[test]
    fn cache_key_distinguishes_transports() {
        let stdio = McpServerConfig {
            name: "a".into(),
            bin: "npx".into(),
            args: vec!["srv".into()],
            ..Default::default()
        };
        assert_eq!(stdio.cache_key(), "stdio npx srv");

        let http = McpServerConfig {
            name: "b".into(),
            transport: Transport::Http,
            url: Some("https://host/mcp".into()),
            ..Default::default()
        };
        assert_eq!(http.cache_key(), "http https://host/mcp");
    }

    #[test]
    fn drain_sse_events_parses_data_lines_across_chunks() {
        // Two events split across a chunk boundary; only the first is complete.
        let mut buf = String::from("event: message\ndata: {\"id\":1}\n\npartial");
        let events = drain_sse_events(&mut buf);
        assert_eq!(events, vec!["{\"id\":1}".to_string()]);
        assert_eq!(buf, "partial");

        // A multi-line `data:` block joins with newlines.
        let mut buf = String::from("data: line1\ndata: line2\n\n");
        assert_eq!(drain_sse_events(&mut buf), vec!["line1\nline2".to_string()]);
        assert!(buf.is_empty());

        // Comments/other fields are ignored; no data -> no event.
        let mut buf = String::from(": keep-alive\n\n");
        assert!(drain_sse_events(&mut buf).is_empty());
    }

    #[test]
    fn parse_tools_sorts_and_reads_names() {
        let resp = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": { "tools": [{ "name": "zeta" }, { "name": "alpha" }, { "other": 1 }] }
        });
        assert_eq!(parse_tools(&resp), vec!["alpha".to_string(), "zeta".to_string()]);

        // Missing/empty result -> empty list, no panic.
        assert!(parse_tools(&json!({ "result": {} })).is_empty());
        assert!(parse_tools(&json!({})).is_empty());
    }

    #[test]
    fn extract_call_result_flattens_text_and_flags_errors() {
        let ok = json!({
            "result": { "content": [{ "type": "text", "text": "hello" }, { "type": "text", "text": "world" }] }
        });
        assert_eq!(
            extract_call_result(&ok)
                .unwrap()
                .get("text")
                .and_then(|t| t.as_str()),
            Some("hello\nworld")
        );

        let err = json!({ "result": { "isError": true }, "error": { "message": "boom" } });
        let e = extract_call_result(&err).unwrap_err();
        assert!(e.contains("boom"), "{e}");
    }

    #[test]
    fn encode_frame_matches_content_length_framing() {
        let frame = encode_frame(&json!({ "jsonrpc": "2.0", "id": 1 })).unwrap();
        let text = String::from_utf8(frame).unwrap();
        let (header, body) = text.split_once("\r\n\r\n").unwrap();
        assert_eq!(header, format!("Content-Length: {}", body.len()));
        let msg: Value = serde_json::from_str(body).unwrap();
        assert_eq!(msg.get("id").and_then(|v| v.as_u64()), Some(1));
        assert_eq!(msg.get("jsonrpc").and_then(|v| v.as_str()), Some("2.0"));
    }

    #[tokio::test]
    async fn read_frame_parses_across_split_chunks() {
        use tokio::io::{AsyncWriteExt, BufReader};
        let (mut client, server) = tokio::io::duplex(64);
        let mut reader = BufReader::new(server);

        let body = r#"{"jsonrpc":"2.0","id":7,"result":{}}"#;
        let header = format!("Content-Length: {}\r\n\r\n", body.len());
        let body_owned = body.to_string();
        // Header and body arrive in separate writes; read_frame must wait.
        tokio::spawn(async move {
            client.write_all(header.as_bytes()).await.unwrap();
            client.write_all(body_owned.as_bytes()).await.unwrap();
        });

        let frame = read_frame_from(&mut reader).await.unwrap();
        assert_eq!(frame.len(), body.len());
        let msg: Value = serde_json::from_str(&frame).unwrap();
        assert_eq!(msg.get("id").and_then(|v| v.as_u64()), Some(7));
    }

    #[tokio::test]
    async fn read_frame_errors_on_eof_and_missing_header() {
        use tokio::io::BufReader;
        // EOF before any header.
        let (client, server) = tokio::io::duplex(64);
        drop(client);
        let mut reader = BufReader::new(server);
        let err = read_frame_from(&mut reader).await.unwrap_err();
        assert!(err.contains("closed the connection"), "{err}");

        // Header with no Content-Length.
        let (mut client, server) = tokio::io::duplex(64);
        let mut reader = BufReader::new(server);
        tokio::spawn(async move {
            client.write_all(b"X-Other: 1\r\n\r\n").await.unwrap();
        });
        let err = read_frame_from(&mut reader).await.unwrap_err();
        assert!(err.contains("missing Content-Length"), "{err}");
    }

    #[tokio::test]
    async fn stdio_handle_reports_error_for_missing_binary() {
        let result = McpHandle::spawn("this-binary-does-not-exist-xyz", &[], &BTreeMap::new()).await;
        let err = match result {
            Ok(_) => panic!("spawn unexpectedly succeeded for a missing binary"),
            Err(e) => e,
        };
        assert!(err.contains("Failed to spawn"), "unexpected error: {err}");
    }
}
