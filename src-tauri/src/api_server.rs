//! Local OpenAI-compatible HTTP API server.
//!
//! Exposes the currently loaded engine pool on `127.0.0.1:{port}`:
//! * `GET  /v1/models` — list the loaded model
//! * `POST /v1/chat/completions` — messages → prompt → completion (non-streaming)
//! * `POST /v1/completions` — raw prompt passthrough
//!
//! Deliberately dependency-free: a tiny hand-rolled HTTP/1.1 loop over a
//! std TcpListener (Content-Length bodies only, `Connection: close`). It is
//! bound to loopback exclusively and never exposed to the network.

use std::io::{Read, Write};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use crate::engine::TextGenerator;

/// Shared engine handle the server reads from; kept in sync by main.rs every
/// time a model is loaded, reconfigured or unloaded.
#[derive(Clone, Default)]
pub struct SharedEngine {
    inner: Arc<Mutex<Option<Arc<crate::engine::EnginePool>>>>,
}

impl SharedEngine {
    pub fn set(&self, pool: Option<Arc<crate::engine::EnginePool>>) {
        *self.inner.lock().unwrap() = pool;
    }

    fn get(&self) -> Option<Arc<crate::engine::EnginePool>> {
        self.inner.lock().unwrap().clone()
    }
}

/// Handle for a running server; dropping it closes the listener.
pub struct ApiServerHandle {
    pub port: u16,
    shutdown: Arc<std::sync::atomic::AtomicBool>,
    // Kept (not read) so the port stays reserved while the handle lives.
    #[allow(dead_code)]
    listener: std::net::TcpListener,
}

impl Drop for ApiServerHandle {
    fn drop(&mut self) {
        self.shutdown
            .store(true, std::sync::atomic::Ordering::SeqCst);
        // Unblock the blocking accept by connecting to ourselves.
        let _ = std::net::TcpStream::connect_timeout(
            &std::net::SocketAddr::from(([127, 0, 0, 1], self.port)),
            std::time::Duration::from_millis(250),
        );
    }
}

/// Bind + spawn the accept loop on a background thread. Returns immediately.
pub fn start(engine: SharedEngine, port: u16) -> Result<ApiServerHandle, String> {
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let listener = std::net::TcpListener::bind(addr)
        .map_err(|e| format!("Cannot bind 127.0.0.1:{port}: {e}"))?;
    let actual_port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let shutdown = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let shutdown_clone = shutdown.clone();
    // Keep a duplicate fd/HANDLE for the caller; the original moves into the
    // accept loop. Dropping the clone unblocks `accept` via the self-connect.
    let listener_for_handle = listener
        .try_clone()
        .map_err(|e| format!("listener clone failed: {e}"))?;

    std::thread::spawn(move || {
        for stream in listener.incoming() {
            if shutdown_clone.load(std::sync::atomic::Ordering::SeqCst) {
                break;
            }
            let Ok(mut stream) = stream else { continue };
            let engine = engine.clone();
            let shutdown = shutdown_clone.clone();
            // One short-lived thread per connection keeps handlers simple.
            std::thread::spawn(move || {
                let _ = handle_connection(&mut stream, &engine, &shutdown);
            });
        }
    });

    Ok(ApiServerHandle {
        port: actual_port,
        shutdown,
        listener: listener_for_handle,
    })
}

fn handle_connection(
    stream: &mut std::net::TcpStream,
    engine: &SharedEngine,
    shutdown: &std::sync::atomic::AtomicBool,
) -> Result<(), String> {
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(10)))
        .map_err(|e| e.to_string())?;

    let request = read_request(stream)?;
    let (method, path) = request;
    if method == "GET" && path == "/v1/models" {
        return write_json(stream, 200, &models_payload(engine));
    }

    let body = read_body(stream)?;
    let payload: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
    if shutdown.load(std::sync::atomic::Ordering::SeqCst) {
        return write_json(
            stream,
            503,
            &json!({ "error": { "message": "shutting down" } }),
        );
    }

    match (method.as_str(), path.as_str()) {
        ("POST", "/v1/chat/completions") => {
            let Some(model_name) = model_name(engine) else {
                return write_json(
                    stream,
                    503,
                    &json!({ "error": { "message": "No model loaded in the editor" } }),
                );
            };
            let prompt = chat_prompt(&payload);
            complete(stream, engine, &model_name, &prompt)
        }
        ("POST", "/v1/completions") => {
            let Some(model_name) = model_name(engine) else {
                return write_json(
                    stream,
                    503,
                    &json!({ "error": { "message": "No model loaded in the editor" } }),
                );
            };
            let prompt = payload
                .get("prompt")
                .and_then(|p| p.as_str())
                .unwrap_or("")
                .to_string();
            complete(stream, engine, &model_name, &prompt)
        }
        _ => write_json(
            stream,
            404,
            &json!({ "error": { "message": format!("No route for {method} {path}") } }),
        ),
    }
}

fn models_payload(engine: &SharedEngine) -> Value {
    let data = engine.get().map(|pool| {
        vec![json!({
            "id": pool.info().name,
            "object": "model",
            "owned_by": "ai-editor",
        })]
    });
    json!({
        "object": "list",
        "data": data.unwrap_or_default(),
    })
}

fn model_name(engine: &SharedEngine) -> Option<String> {
    engine.get().map(|pool| pool.info().name)
}

fn complete(
    stream: &mut std::net::TcpStream,
    engine: &SharedEngine,
    model: &str,
    prompt: &str,
) -> Result<(), String> {
    let Some(pool) = engine.get() else {
        return write_json(
            stream,
            503,
            &json!({ "error": { "message": "No model loaded" } }),
        );
    };

    let max_tokens = pool.info().context_size.clamp(64, 2048);
    let request = crate::engine::InferenceRequest {
        prompt: prompt.to_string(),
        messages: None,
        max_tokens,
        temperature: None,
        top_p: None,
        repeat_penalty: None,
        seed: None,
        stop_words: None,
        cached_prefix_tokens: None,
    };
    let interrupt = CancellationToken::new();
    // Session id 0: API completions must not render into any UI chat stream.
    let mut generator = pool.handle(0);
    let outcome = generator.generate(&request, 0, &interrupt, &dummy_sender())?;

    let usage = json!({
        "prompt_tokens": outcome.done.input_tokens,
        "completion_tokens": outcome.done.output_tokens,
        "total_tokens": outcome.done.total_tokens,
    });
    let response = json!({
        "id": format!("chatcmpl-{}", uuid_short()),
        "object": "chat.completion",
        "created": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
        "model": model,
        "choices": [{
            "index": 0,
            "message": { "role": "assistant", "content": outcome.full_text },
            "finish_reason": "stop",
        }],
        "usage": usage,
    });
    write_json(stream, 200, &response)
}

/// Flatten OpenAI chat `messages` into the plain-text prompt style our local
/// models were tuned with (`User:` / `Assistant:` turns + optional system).
fn chat_prompt(payload: &Value) -> String {
    let mut system = String::new();
    let mut turns = String::new();
    if let Some(messages) = payload.get("messages").and_then(|m| m.as_array()) {
        for msg in messages {
            let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("");
            let content = msg.get("content").and_then(|c| c.as_str()).unwrap_or("");
            match role {
                "system" => {
                    system.push_str(content.trim());
                    system.push('\n');
                }
                "user" => turns.push_str(&format!("User: {content}\n")),
                "assistant" => turns.push_str(&format!("Assistant: {content}\n")),
                _ => {}
            }
        }
    }
    format!("{system}\n{turns}Assistant:")
}

// --- minimal HTTP plumbing ---------------------------------------------------

const MAX_BODY: usize = 8 * 1024 * 1024;

type RequestLine = (String, String);

fn read_request(stream: &mut std::net::TcpStream) -> Result<RequestLine, String> {
    let mut buf = [0u8; 1024];
    let mut head = Vec::new();
    loop {
        let n = stream
            .read(&mut buf)
            .map_err(|e| format!("read failed: {e}"))?;
        if n == 0 {
            return Err("connection closed before headers".into());
        }
        head.extend_from_slice(&buf[..n]);
        if let Some(pos) = find_double_crlf(&head) {
            let text = String::from_utf8_lossy(&head[..pos]).to_string();
            let mut lines = text.lines();
            let request_line = lines.next().unwrap_or("").to_string();
            let mut parts = request_line.split_whitespace();
            let method = parts.next().unwrap_or("").to_uppercase();
            let path = parts.next().unwrap_or("/").to_string();
            return Ok((method, path));
        }
        if head.len() > 32 * 1024 {
            return Err("headers too large".into());
        }
    }
}

fn read_body(stream: &mut std::net::TcpStream) -> Result<String, String> {
    let mut all = Vec::new();
    // Re-read whatever is left (headers may already contain part of the body).
    let mut buf = [0u8; 8192];
    loop {
        let n = stream
            .read(&mut buf)
            .map_err(|e| format!("read failed: {e}"))?;
        if n == 0 {
            break;
        }
        all.extend_from_slice(&buf[..n]);
        if let Some(pos) = find_double_crlf(&all) {
            let header_end = pos + 4;
            let headers = String::from_utf8_lossy(&all[..pos]).to_lowercase();
            let content_length = headers
                .lines()
                .find_map(|l| l.strip_prefix("content-length:"))
                .and_then(|v| v.trim().parse::<usize>().ok())
                .unwrap_or(0);
            if content_length > MAX_BODY {
                return Err("body too large".into());
            }
            let mut body: Vec<u8> = all[header_end..].to_vec();
            while body.len() < content_length {
                let n = stream
                    .read(&mut buf)
                    .map_err(|e| format!("read failed: {e}"))?;
                if n == 0 {
                    break;
                }
                body.extend_from_slice(&buf[..n]);
            }
            body.truncate(content_length);
            return Ok(String::from_utf8_lossy(&body).into_owned());
        }
        if all.len() > 32 * 1024 {
            return Err("headers too large".into());
        }
    }
    Ok(String::new())
}

fn find_double_crlf(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

fn write_json(stream: &mut std::net::TcpStream, status: u16, body: &Value) -> Result<(), String> {
    let bytes = serde_json::to_vec(body).map_err(|e| e.to_string())?;
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        503 => "Service Unavailable",
        _ => "Error",
    };
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        bytes.len()
    );
    stream
        .write_all(response.as_bytes())
        .map_err(|e| e.to_string())?;
    stream.write_all(&bytes).map_err(|e| e.to_string())?;
    stream.flush().map_err(|e| e.to_string())
}

/// A throwaway crossbeam sender that swallows worker events (API generations
/// are invisible to the UI event bus).
fn dummy_sender() -> crossbeam_channel::Sender<crate::engine::WorkerEvent> {
    let (tx, _) = crossbeam_channel::unbounded();
    tx
}

fn uuid_short() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64 ^ d.as_secs())
        .unwrap_or(0);
    format!("{nanos:08x}")
}
