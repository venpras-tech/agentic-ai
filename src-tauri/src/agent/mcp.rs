//! Minimal stdio MCP (Model Context Protocol) client.
//!
//! Speaks JSON-RPC 2.0 over the server's stdin/stdout. Each server connection
//! performs the `initialize` handshake once, then serves `tools/call`
//! requests. This lets the orchestrator extend itself with any MCP server the
//! user installs (playwright, puppeteer, filesystem, postgres, …) without any
//! code changes.

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter};

/// A live connection to a stdio MCP server. Not `Sync`; wrap in a mutex.
pub struct McpHandle {
    stdin: tokio::process::ChildStdin,
    stdout: BufReader<tokio::process::ChildStdout>,
    next_id: u32,
}

impl McpHandle {
    /// Spawn `bin args…` and complete the JSON-RPC `initialize` handshake.
    pub async fn spawn(bin: &str, args: &[String]) -> Result<McpHandle, String> {
        let mut cmd = tokio::process::Command::new(bin);
        cmd.args(args)
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
        let stdin = child.stdin.take().ok_or("MCP server did not expose stdin")?;
        let stdout = child.stdout.take().ok_or("MCP server did not expose stdout")?;
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
            let msg: Value = serde_json::from_str(&raw).map_err(|e| format!("Bad JSON-RPC message: {e}"))?;
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
        buf.write_all(header.as_bytes()).await.map_err(|e| e.to_string())?;
        buf.write_all(&bytes).await.map_err(|e| e.to_string())?;
        buf.flush().await.map_err(|e| e.to_string())
    }

    async fn read_frame(&mut self) -> Result<String, String> {
        // Read the Content-Length header
        let mut content_length: Option<usize> = None;
        loop {
            let mut line = String::new();
            self.stdout.read_line(&mut line).await.map_err(|e| e.to_string())?;
            if line.is_empty() {
                return Err("MCP server closed the connection".to_string());
            }
            let trimmed = line.trim_end_matches(['\r', '\n']);
            if trimmed.is_empty() {
                break;
            }
            if let Some(v) = trimmed.strip_prefix("Content-Length:") {
                content_length = Some(v.trim().parse().map_err(|_| "Bad Content-Length header".to_string())?);
            }
        }
        let len = content_length.ok_or("MCP frame missing Content-Length header")?;
        if len > 64 * 1024 * 1024 {
            return Err("MCP frame too large".to_string());
        }
        let mut body = vec![0u8; len];
        use tokio::io::AsyncReadExt;
        self.stdout.read_exact(&mut body).await.map_err(|e| e.to_string())?;
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
