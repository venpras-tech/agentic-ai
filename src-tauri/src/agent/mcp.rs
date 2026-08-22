//! Minimal stdio MCP (Model Context Protocol) client.
//!
//! Speaks JSON-RPC 2.0 over the server's stdin/stdout. Each server connection
//! performs the `initialize` handshake once, then serves `tools/call`
//! requests. This lets the orchestrator extend itself with any MCP server the
//! user installs (playwright, puppeteer, filesystem, postgres, …) without any
//! code changes.
//!
//! A persisted *catalog* (`{config}/mcp-servers.json`) holds named server
//! configs so the model can call them by name and the user can manage them
//! from the settings UI without re-entering command lines every session.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter};

/// One configured MCP server in the user's catalog.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServerConfig {
    /// Short unique identifier used in tool calls (`"server": "playwright"`).
    pub name: String,
    /// Executable (PATH lookup or absolute path), stdio transport.
    pub bin: String,
    /// Command-line arguments passed to the executable.
    #[serde(default)]
    pub args: Vec<String>,
    /// Extra environment variables for the spawned process.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
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
    pub fn tool_allowed(&self, tool: &str) -> bool {
        Self::matches_allow_list(&self.allowed_tools, tool)
    }

    /// Allow-list matcher shared with ad-hoc calls (empty list = allow all).
    pub fn matches_allow_list(list: &[String], tool: &str) -> bool {
        if list.is_empty() {
            return true;
        }
        list.iter().any(|entry| match entry.strip_suffix('*') {
            Some(prefix) => tool.starts_with(prefix),
            None => entry == tool,
        })
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
            let mcp_err = resp
                .get("error")
                .map(|e| e.to_string())
                .unwrap_or_else(|| "MCP tool reported an error".to_string());
            return Err(mcp_err);
        }
        Ok(json!({ "text": text }))
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
        let bytes = serde_json::to_vec(value).map_err(|e| e.to_string())?;
        let mut buf = BufWriter::new(&mut self.stdin);
        // Content-Length framing as used by MCP stdio transport
        let header = format!("Content-Length: {}\r\n\r\n", bytes.len());
        buf.write_all(header.as_bytes())
            .await
            .map_err(|e| e.to_string())?;
        buf.write_all(&bytes).await.map_err(|e| e.to_string())?;
        buf.flush().await.map_err(|e| e.to_string())
    }

    async fn read_frame(&mut self) -> Result<String, String> {
        // Read the Content-Length header
        let mut content_length: Option<usize> = None;
        loop {
            let mut line = String::new();
            self.stdout
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
        use tokio::io::AsyncReadExt;
        self.stdout
            .read_exact(&mut body)
            .await
            .map_err(|e| e.to_string())?;
        String::from_utf8(body).map_err(|e| e.to_string())
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
            },
            McpServerConfig {
                name: "fs".into(),
                bin: "mcp-server-filesystem".into(),
                args: vec!["D:\\data".into()],
                env: BTreeMap::new(),
                allowed_tools: Vec::new(),
                enabled: false,
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
        };
        assert!(open.tool_allowed("anything"));

        let strict = McpServerConfig {
            name: "fs".into(),
            bin: "x".into(),
            args: vec![],
            env: BTreeMap::new(),
            allowed_tools: vec!["read_file".into()],
            enabled: true,
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
}
