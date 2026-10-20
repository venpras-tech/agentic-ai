//! Local OpenAI-compatible HTTP API server, Ollama / LM Studio style.
//!
//! Exposes the currently loaded engine pool on `127.0.0.1:{port}`:
//! * `GET  /v1/models` — list the loaded model
//! * `POST /v1/chat/completions` — OpenAI chat messages → completion, with
//!   optional SSE streaming (`stream: true`) and chat-template rendering
//! * `POST /v1/completions` — raw prompt passthrough (also supports SSE)
//! * `POST /v1/responses` — OpenAI Responses API translation shim (also SSE),
//!   so Responses-only clients such as Codex CLI can talk to a local model
//! * `POST /v1/agent/tasks` — run an agentic task (mirrors `agent_run_task`);
//!   returns `{ "sessionId": <n> }` immediately. Subscribe to the `agent://*`
//!   events (or poll `/v1/agent/sessions/{id}/trace`) for progress.
//! * `GET  /v1/agent/sessions/{id}/trace` — the recorded per-step run trace
//!   for a session as a JSON array.
//!
//! The chat route renders `messages` through the model's own chat template
//! (exactly like the agent loop), so Ollama/LM Studio clients get the same
//! instruction format the model was tuned with. Endpoint and payload shape are
//! OpenAI-compatible so any Ollama/LM Studio client can point at it.
//!
//! Deliberately dependency-free: a tiny hand-rolled HTTP/1.1 loop over a
//! std TcpListener (Content-Length bodies only, `Connection: close`). It is
//! bound to loopback exclusively and never exposed to the network.

use std::io::{Read, Write};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use tauri::{AppHandle, Manager};
use tokio_util::sync::CancellationToken;

use crate::agent::interrupt::InterruptState;
use crate::agent::orchestrator::AgentTaskRequest;
use crate::engine::TextGenerator;
use crate::{ContextState, InferenceState, ProviderRegistryState};

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
///
/// `app` is kept so the `POST /v1/agent/tasks` route can build the managed
/// agent `State` handles; every other route only needs the engine pool.
pub fn start(engine: SharedEngine, port: u16, app: AppHandle) -> Result<ApiServerHandle, String> {
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
            let app = app.clone();
            // One short-lived thread per connection keeps handlers simple.
            std::thread::spawn(move || {
                let _ = handle_connection(&mut stream, &engine, &shutdown, &app);
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
    app: &AppHandle,
) -> Result<(), String> {
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(10)))
        .map_err(|e| e.to_string())?;

    let request = read_request(stream)?;
    let (method, path) = request;
    if method == "GET" && path == "/v1/models" {
        return write_json(stream, 200, &models_payload(engine));
    }
    // GET routes carry no body — answer them before `read_body` (which would
    // otherwise block until the read timeout waiting for a Content-Length).
    if method == "GET" {
        if let Some(session_id) = parse_trace_path(&path) {
            return agent_trace(stream, app, session_id);
        }
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
        ("POST", "/v1/agent/tasks") => agent_task(stream, app, &payload),
        ("POST", "/v1/chat/completions") => {
            let wants_stream = payload
                .get("stream")
                .and_then(|s| s.as_bool())
                .unwrap_or(false);
            if wants_stream {
                chat_stream(stream, engine, &payload)
            } else {
                chat_complete(stream, engine, &payload)
            }
        }
        ("POST", "/v1/completions") => {
            let wants_stream = payload
                .get("stream")
                .and_then(|s| s.as_bool())
                .unwrap_or(false);
            if wants_stream {
                complete_stream(stream, engine, &payload)
            } else {
                complete(stream, engine, &payload)
            }
        }
        ("POST", "/v1/responses") => {
            let translated = responses_to_chat(&payload)?;
            let wants_stream = payload
                .get("stream")
                .and_then(|s| s.as_bool())
                .unwrap_or(false);
            if wants_stream {
                responses_stream(stream, engine, &payload, &translated)
            } else {
                responses_complete(stream, engine, &payload, &translated)
            }
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

/// Parse a `/v1/agent/sessions/{id}/trace` path into its numeric session id.
fn parse_trace_path(path: &str) -> Option<u64> {
    path.strip_prefix("/v1/agent/sessions/")?
        .strip_suffix("/trace")?
        .parse::<u64>()
        .ok()
}

/// `POST /v1/agent/tasks` — run an agentic task by delegating to the same core
/// as the `agent_run_task` command. Returns the session id immediately; the run
/// proceeds on its own thread and can be followed via the `agent://*` events or
/// `/v1/agent/sessions/{id}/trace`.
fn agent_task(
    stream: &mut std::net::TcpStream,
    app: &AppHandle,
    payload: &Value,
) -> Result<(), String> {
    let request: AgentTaskRequest = match serde_json::from_value(payload.clone()) {
        Ok(r) => r,
        Err(e) => {
            return write_json(
                stream,
                400,
                &json!({ "error": { "message": format!("invalid task body: {e}") } }),
            )
        }
    };
    let app = app.clone();
    let result = tauri::async_runtime::block_on(async move {
        let state = app.state::<InferenceState>();
        let providers = app.state::<ProviderRegistryState>();
        let interrupt = app.state::<InterruptState>();
        let context = app.state::<ContextState>();
        let tool_state = app.state::<Arc<crate::agent::ToolState>>();
        crate::commands::agent::run_agent_task_inner(
            app.clone(),
            &state,
            &providers,
            &interrupt,
            &context,
            &tool_state,
            request,
        )
        .await
    });
    match result {
        Ok(session_id) => write_json(stream, 200, &json!({ "sessionId": session_id })),
        Err(e) => write_json(stream, 500, &json!({ "error": { "message": e } })),
    }
}

/// `GET /v1/agent/sessions/{id}/trace` — the recorded per-step run trace.
fn agent_trace(
    stream: &mut std::net::TcpStream,
    app: &AppHandle,
    session_id: u64,
) -> Result<(), String> {
    let tool_state = app.state::<Arc<crate::agent::ToolState>>();
    let trace = tool_state.traces_for(session_id);
    write_json(
        stream,
        200,
        &json!({ "sessionId": session_id, "trace": trace }),
    )
}

/// Convert OpenAI chat `messages` into engine turns. Handles both plain-text
/// content and the array-of-blocks form (`{"type":"text","text":…}`).
fn chat_turns(payload: &Value) -> Vec<crate::engine::ChatTurn> {
    let mut turns = Vec::new();
    if let Some(messages) = payload.get("messages").and_then(|m| m.as_array()) {
        for msg in messages {
            let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("user");
            let content = match msg.get("content") {
                Some(Value::String(s)) => s.clone(),
                Some(Value::Array(blocks)) => blocks
                    .iter()
                    .filter_map(|b| b.get("text").and_then(|t| t.as_str()).map(str::to_string))
                    .collect::<Vec<_>>()
                    .join("\n"),
                _ => String::new(),
            };
            if !content.trim().is_empty() {
                turns.push(crate::engine::ChatTurn {
                    role: role.to_string(),
                    content,
                });
            }
        }
    }
    turns
}

/// Build the engine request from an OpenAI `chat/completions` payload.
fn chat_request(payload: &Value) -> Result<crate::engine::InferenceRequest, String> {
    let turns = chat_turns(payload);
    if turns.is_empty() {
        return Err("chat/completions requires a non-empty `messages` array".into());
    }
    let max_tokens = payload
        .get("max_tokens")
        .or_else(|| payload.get("max_completion_tokens"))
        .and_then(|v| v.as_u64())
        .map(|v| v.clamp(1, 16384) as u32)
        .unwrap_or(2048);
    let stop_words = match payload.get("stop") {
        Some(Value::String(s)) => Some(vec![s.clone()]),
        Some(Value::Array(arr)) => {
            let v: Vec<String> = arr
                .iter()
                .filter_map(|s| s.as_str().map(str::to_string))
                .collect();
            if v.is_empty() {
                None
            } else {
                Some(v)
            }
        }
        _ => None,
    };
    Ok(crate::engine::InferenceRequest {
        prompt: String::new(),
        messages: Some(turns),
        images: None,
        max_tokens,
        stop_words,
        cached_prefix_tokens: None,
        ..Default::default()
    })
}

/// Build the engine request from an OpenAI `completions` payload (raw prompt).
fn completions_request(payload: &Value) -> Result<crate::engine::InferenceRequest, String> {
    let prompt = payload
        .get("prompt")
        .and_then(|p| p.as_str())
        .unwrap_or("")
        .to_string();
    if prompt.trim().is_empty() {
        return Err("completions requires a `prompt` string".into());
    }
    let max_tokens = payload
        .get("max_tokens")
        .and_then(|v| v.as_u64())
        .map(|v| v.clamp(1, 16384) as u32)
        .unwrap_or(256);
    Ok(crate::engine::InferenceRequest {
        prompt,
        messages: None,
        images: None,
        max_tokens,
        stop_words: None,
        cached_prefix_tokens: None,
        ..Default::default()
    })
}

/// Resolve the effective prompt for a request that carries `messages`: render
/// through the model's chat template on the worker; if the model has none,
/// flatten to the plain-text fallback style.
fn resolve_prompt(
    pool: &Arc<crate::engine::EnginePool>,
    mut request: crate::engine::InferenceRequest,
    fallback: String,
) -> crate::engine::InferenceRequest {
    if let Some(msgs) = request.messages.as_ref() {
        if !msgs.is_empty() {
            let gen = pool.handle(0);
            let rendered = gen.render_chat(&request).unwrap_or(fallback);
            request.prompt = rendered;
            request.messages = None;
        }
    }
    request
}

/// Terminal response shell shared by the non-streaming chat route.
fn chat_response(model: &str, full_text: &str, outcome: &crate::engine::InferenceDone) -> Value {
    json!({
        "id": format!("chatcmpl-{}", uuid_short()),
        "object": "chat.completion",
        "created": now_secs(),
        "model": model,
        "choices": [{
            "index": 0,
            "message": { "role": "assistant", "content": full_text },
            "finish_reason": "stop",
        }],
        "usage": usage(outcome),
    })
}

fn usage(done: &crate::engine::InferenceDone) -> Value {
    json!({
        "prompt_tokens": done.input_tokens,
        "completion_tokens": done.output_tokens,
        "total_tokens": done.total_tokens,
    })
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Non-streaming `POST /v1/chat/completions`.
fn chat_complete(
    stream: &mut std::net::TcpStream,
    engine: &SharedEngine,
    payload: &Value,
) -> Result<(), String> {
    let Some(pool) = engine.get() else {
        return write_json(
            stream,
            503,
            &json!({ "error": { "message": "No model loaded" } }),
        );
    };
    let model = pool.info().name;
    let request = chat_request(payload)?;
    let request = resolve_prompt(&pool, request, chat_prompt(payload));
    let outcome = generate_nonstreaming(&pool, request)?;
    write_json(
        stream,
        200,
        &chat_response(&model, &outcome.full_text, &outcome.done),
    )
}

/// Non-streaming `POST /v1/completions`.
fn complete(
    stream: &mut std::net::TcpStream,
    engine: &SharedEngine,
    payload: &Value,
) -> Result<(), String> {
    let Some(pool) = engine.get() else {
        return write_json(
            stream,
            503,
            &json!({ "error": { "message": "No model loaded" } }),
        );
    };
    let model = pool.info().name;
    let request = completions_request(payload)?;
    let outcome = generate_nonstreaming(&pool, request)?;
    let response = json!({
        "id": format!("cmpl-{}", uuid_short()),
        "object": "text_completion",
        "created": now_secs(),
        "model": model,
        "choices": [{
            "index": 0,
            "text": outcome.full_text,
            "finish_reason": "stop",
        }],
        "usage": usage(&outcome.done),
    });
    write_json(stream, 200, &response)
}

fn generate_nonstreaming(
    pool: &Arc<crate::engine::EnginePool>,
    request: crate::engine::InferenceRequest,
) -> Result<crate::engine::GenerationOutcome, String> {
    let interrupt = CancellationToken::new();
    // Session id 0: API completions must not render into any UI chat stream.
    pool.handle(0)
        .generate(&request, 0, &interrupt, &dummy_sender())
}

/// Streaming `POST /v1/chat/completions` (SSE). Chunk shape matches the
/// OpenAI/llama.cpp-server format so Ollama/LM Studio clients parse it as-is.
fn chat_stream(
    stream: &mut std::net::TcpStream,
    engine: &SharedEngine,
    payload: &Value,
) -> Result<(), String> {
    let Some(pool) = engine.get() else {
        return write_json(
            stream,
            503,
            &json!({ "error": { "message": "No model loaded" } }),
        );
    };
    let model = pool.info().name;
    let request = chat_request(payload)?;
    let request = resolve_prompt(&pool, request, chat_prompt(payload));
    stream_request(
        stream,
        &pool,
        request,
        |delta, is_first| {
            if is_first {
                json!({
                    "id": format!("chatcmpl-{}", uuid_short()),
                    "object": "chat.completion.chunk",
                    "created": now_secs(),
                    "model": model,
                    "choices": [{
                        "index": 0,
                        "delta": { "role": "assistant", "content": delta },
                        "finish_reason": null,
                    }],
                })
            } else {
                json!({
                    "id": format!("chatcmpl-{}", uuid_short()),
                    "object": "chat.completion.chunk",
                    "created": now_secs(),
                    "model": model,
                    "choices": [{
                        "index": 0,
                        "delta": { "content": delta },
                        "finish_reason": null,
                    }],
                })
            }
        },
        |_content, outcome| {
            json!({
                "id": format!("chatcmpl-{}", uuid_short()),
                "object": "chat.completion.chunk",
                "created": now_secs(),
                "model": model,
                "choices": [{
                    "index": 0,
                    "delta": {},
                    "finish_reason": "stop",
                }],
                "usage": usage(outcome),
            })
        },
    )
}

/// Streaming `POST /v1/completions` (SSE).
fn complete_stream(
    stream: &mut std::net::TcpStream,
    engine: &SharedEngine,
    payload: &Value,
) -> Result<(), String> {
    let Some(pool) = engine.get() else {
        return write_json(
            stream,
            503,
            &json!({ "error": { "message": "No model loaded" } }),
        );
    };
    let model = pool.info().name;
    let request = completions_request(payload)?;
    stream_request(
        stream,
        &pool,
        request,
        |delta, _is_first| {
            json!({
                "id": format!("cmpl-{}", uuid_short()),
                "object": "text_completion",
                "created": now_secs(),
                "model": model,
                "choices": [{ "index": 0, "text": delta, "finish_reason": null }],
            })
        },
        |_content, outcome| {
            json!({
                "id": format!("cmpl-{}", uuid_short()),
                "object": "text_completion",
                "created": now_secs(),
                "model": model,
                "choices": [{ "index": 0, "text": "", "finish_reason": "stop" }],
                "usage": usage(outcome),
            })
        },
    )
}

/// Shared SSE pipeline: stream token deltas as OpenAI chunks, then emit a
/// terminal chunk carrying the usage + a final `data: [DONE]` line.
fn stream_request(
    stream: &mut std::net::TcpStream,
    pool: &Arc<crate::engine::EnginePool>,
    request: crate::engine::InferenceRequest,
    chunk: impl Fn(&str, bool) -> Value,
    finish_chunk: impl Fn(String, &crate::engine::InferenceDone) -> Value,
) -> Result<(), String> {
    let interrupt = CancellationToken::new();
    let (events, outcomes) = pool.handle(0).stream(request, 0, interrupt)?;
    write_sse_headers(stream)?;

    let mut sent_first = false;
    let mut full_text = String::new();
    loop {
        match events.recv() {
            Ok(crate::engine::WorkerEvent::Token { delta, .. }) => {
                full_text.push_str(&delta);
                let body = chunk(&delta, !sent_first);
                sent_first = true;
                write_sse(stream, &body)?;
            }
            Ok(crate::engine::WorkerEvent::Done { .. }) => break,
            Ok(_) => {}
            Err(_) => break,
        }
    }
    // The worker sends the terminal stats right after Done — take them or fall
    // back to a zeroed accounting so the closing chunk is still valid JSON.
    let outcome = outcomes
        .recv_timeout(std::time::Duration::from_secs(5))
        .ok()
        .and_then(|r| r.ok());
    let done =
        outcome
            .as_ref()
            .map(|o| o.done.clone())
            .unwrap_or_else(|| crate::engine::InferenceDone {
                total_tokens: 0,
                generated_chars: full_text.chars().count() as u64,
                tokens_per_sec: 0.0,
                elapsed_ms: 0,
                stop_reason: "stop".into(),
                outcome: "completed".into(),
                input_tokens: 0,
                output_tokens: full_text.chars().count() as u64,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
                reasoning_tokens: 0,
            });
    write_sse(stream, &finish_chunk(full_text, &done))?;
    write_sse_done(stream)
}

// --- /v1/responses (OpenAI Responses API) -------------------------------------
//
// Codex CLI and other Responses-only clients post to `/v1/responses` and never
// touch `/v1/chat/completions`. Local llama.cpp models only speak the chat
// shape, so this is a translation layer: request -> chat payload, chat reply ->
// Responses envelope. Standard field names are preserved; only the envelope
// differs.

/// Translate a Responses API request body into the equivalent chat payload.
///
/// Accepts `input` as a plain string, a single message object, or the array
/// form. Content may be a string or the `[{type, text}]` block array
/// (`input_text` / `output_text` / `text`).
fn responses_to_chat(payload: &Value) -> Result<Value, String> {
    let mut messages: Vec<Value> = Vec::new();

    if let Some(instructions) = payload.get("instructions").and_then(|v| v.as_str()) {
        if !instructions.trim().is_empty() {
            messages.push(json!({ "role": "system", "content": instructions }));
        }
    }

    let text_of = |content: &Value| -> String {
        match content {
            Value::String(s) => s.clone(),
            Value::Array(blocks) => blocks
                .iter()
                .filter(|b| {
                    matches!(
                        b.get("type").and_then(|t| t.as_str()),
                        Some("input_text") | Some("output_text") | Some("text")
                    )
                })
                .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
                .collect::<Vec<_>>()
                .join("\n"),
            _ => String::new(),
        }
    };

    match payload.get("input") {
        Some(Value::String(s)) => {
            if !s.trim().is_empty() {
                messages.push(json!({ "role": "user", "content": s }));
            }
        }
        Some(Value::Object(obj)) => {
            let role = obj.get("role").and_then(|r| r.as_str()).unwrap_or("user");
            let content = obj.get("content").map(text_of).unwrap_or_default();
            if !content.trim().is_empty() {
                messages.push(json!({ "role": role, "content": content }));
            }
        }
        Some(Value::Array(items)) => {
            for item in items {
                let Some(obj) = item.as_object() else {
                    continue;
                };
                // Skip function-call / function-call-output items: the local
                // model has no tool channel, so replaying them as text would
                // inject protocol noise into the prompt.
                match obj.get("type").and_then(|t| t.as_str()) {
                    Some("function_call") | Some("function_call_output") | Some("reasoning") => {
                        continue
                    }
                    _ => {}
                }
                let role = obj.get("role").and_then(|r| r.as_str()).unwrap_or("user");
                let content = obj.get("content").map(text_of).unwrap_or_default();
                if !content.trim().is_empty() {
                    messages.push(json!({ "role": role, "content": content }));
                }
            }
        }
        _ => {}
    }

    if messages.is_empty() {
        return Err("responses requires a non-empty `input`".into());
    }

    let mut chat = json!({ "messages": messages });
    if let Some(model) = payload.get("model").and_then(|m| m.as_str()) {
        chat["model"] = json!(model);
    }
    // `max_output_tokens` is the Responses spelling of `max_tokens`.
    for src in ["max_output_tokens", "max_tokens"] {
        if let Some(v) = payload.get(src).and_then(|v| v.as_u64()) {
            chat["max_tokens"] = json!(v);
            break;
        }
    }
    if let Some(stop) = payload.get("stop") {
        chat["stop"] = stop.clone();
    }
    Ok(chat)
}

/// Build the non-streaming Responses envelope around the generated text.
fn responses_body(model: &str, text: &str, outcome: &crate::engine::InferenceDone) -> Value {
    json!({
        "id": format!("resp_{}", uuid_short()),
        "object": "response",
        "created_at": now_secs(),
        "status": "completed",
        "model": model,
        "output": [{
            "id": format!("msg_{}", uuid_short()),
            "type": "message",
            "role": "assistant",
            "status": "completed",
            "content": [{
                "type": "output_text",
                "text": text,
                "annotations": [],
            }],
        }],
        "usage": {
            "input_tokens": outcome.input_tokens,
            "output_tokens": outcome.output_tokens,
            "total_tokens": outcome.total_tokens,
        },
    })
}

fn responses_complete(
    stream: &mut std::net::TcpStream,
    engine: &SharedEngine,
    original: &Value,
    chat: &Value,
) -> Result<(), String> {
    let Some(pool) = engine.get() else {
        return write_json(
            stream,
            503,
            &json!({ "error": { "message": "No model loaded" } }),
        );
    };
    let model = original
        .get("model")
        .and_then(|m| m.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| pool.info().name);
    let request = chat_request(chat)?;
    let request = resolve_prompt(&pool, request, chat_prompt(chat));
    let outcome = generate_nonstreaming(&pool, request)?;
    write_json(
        stream,
        200,
        &responses_body(&model, &outcome.full_text, &outcome.done),
    )
}

/// Streaming `/v1/responses`: SSE `response.*` events over the chat pipeline.
fn responses_stream(
    stream: &mut std::net::TcpStream,
    engine: &SharedEngine,
    original: &Value,
    chat: &Value,
) -> Result<(), String> {
    let Some(pool) = engine.get() else {
        return write_json(
            stream,
            503,
            &json!({ "error": { "message": "No model loaded" } }),
        );
    };
    let model = original
        .get("model")
        .and_then(|m| m.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| pool.info().name);
    let resp_id = format!("resp_{}", uuid_short());
    let msg_id = format!("msg_{}", uuid_short());
    let item_id = msg_id.clone();
    let request = chat_request(chat)?;
    let request = resolve_prompt(&pool, request, chat_prompt(chat));

    let interrupt = CancellationToken::new();
    let (events, outcomes) = pool.handle(0).stream(request, 0, interrupt)?;
    write_sse_headers(stream)?;

    let mut seq: u32 = 0;
    let mut created_body = responses_body(
        &model,
        "",
        &crate::engine::InferenceDone {
            total_tokens: 0,
            generated_chars: 0,
            tokens_per_sec: 0.0,
            elapsed_ms: 0,
            stop_reason: "stop".into(),
            outcome: "in_progress".into(),
            input_tokens: 0,
            output_tokens: 0,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            reasoning_tokens: 0,
        },
    );
    created_body["id"] = json!(resp_id);
    created_body["status"] = json!("in_progress");
    created_body["output"] = json!([]);
    let mut in_progress = created_body.clone();
    in_progress["status"] = json!("in_progress");

    write_sse(
        stream,
        &json!({ "type": "response.created", "response": created_body.clone(), "sequence_number": seq }),
    )?;
    seq += 1;
    write_sse(
        stream,
        &json!({ "type": "response.in_progress", "response": created_body.clone(), "sequence_number": seq }),
    )?;
    seq += 1;

    let mut full_text = String::new();
    write_sse(
        stream,
        &json!({
            "type": "response.output_item.added",
            "output_index": 0,
            "item": {
                "id": item_id,
                "type": "message",
                "role": "assistant",
                "status": "in_progress",
                "content": [],
            },
            "sequence_number": seq,
        }),
    )?;
    seq += 1;
    write_sse(
        stream,
        &json!({
            "type": "response.content_part.added",
            "output_index": 0,
            "item_id": item_id,
            "content_index": 0,
            "part": { "type": "output_text", "text": "" },
            "sequence_number": seq,
        }),
    )?;
    seq += 1;

    loop {
        match events.recv() {
            Ok(crate::engine::WorkerEvent::Token { delta, .. }) => {
                full_text.push_str(&delta);
                write_sse(
                    stream,
                    &json!({
                        "type": "response.output_text.delta",
                        "item_id": item_id,
                        "output_index": 0,
                        "content_index": 0,
                        "delta": delta,
                        "sequence_number": seq,
                    }),
                )?;
                seq += 1;
            }
            Ok(crate::engine::WorkerEvent::Done { .. }) => break,
            Ok(_) => {}
            Err(_) => break,
        }
    }

    let outcome = outcomes
        .recv_timeout(std::time::Duration::from_secs(5))
        .ok()
        .and_then(|r| r.ok());
    let done =
        outcome
            .as_ref()
            .map(|o| o.done.clone())
            .unwrap_or_else(|| crate::engine::InferenceDone {
                total_tokens: 0,
                generated_chars: full_text.chars().count() as u64,
                tokens_per_sec: 0.0,
                elapsed_ms: 0,
                stop_reason: "stop".into(),
                outcome: "completed".into(),
                input_tokens: 0,
                output_tokens: full_text.chars().count() as u64,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
                reasoning_tokens: 0,
            });

    write_sse(
        stream,
        &json!({
            "type": "response.output_text.done",
            "item_id": item_id,
            "output_index": 0,
            "content_index": 0,
            "text": full_text,
            "sequence_number": seq,
        }),
    )?;
    seq += 1;
    write_sse(
        stream,
        &json!({
            "type": "response.content_part.done",
            "output_index": 0,
            "item_id": item_id,
            "content_index": 0,
            "part": { "type": "output_text", "text": full_text },
            "sequence_number": seq,
        }),
    )?;
    seq += 1;
    write_sse(
        stream,
        &json!({
            "type": "response.output_item.done",
            "output_index": 0,
            "item": {
                "id": item_id,
                "type": "message",
                "role": "assistant",
                "status": "completed",
                "content": [{ "type": "output_text", "text": full_text, "annotations": [] }],
            },
            "sequence_number": seq,
        }),
    )?;
    seq += 1;

    let mut body = responses_body(&model, &full_text, &done);
    body["id"] = json!(resp_id);
    body["output"][0]["id"] = json!(item_id);
    write_sse(
        stream,
        &json!({ "type": "response.completed", "response": body, "sequence_number": seq }),
    )?;
    write_sse_done(stream)
}

/// Flatten OpenAI chat `messages` into the plain-text prompt style our local
/// models were tuned with (`User:` / `Assistant:` turns + optional system).
/// Only used when the model ships no chat template.
fn chat_prompt(payload: &Value) -> String {
    let mut system = String::new();
    let mut turns = String::new();
    for turn in chat_turns(payload) {
        match turn.role.as_str() {
            "system" => {
                system.push_str(turn.content.trim());
                system.push('\n');
            }
            "user" => turns.push_str(&format!("User: {}\n", turn.content)),
            "assistant" => turns.push_str(&format!("Assistant: {}\n", turn.content)),
            _ => {}
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

fn write_sse_headers(stream: &mut std::net::TcpStream) -> Result<(), String> {
    stream
        .write_all(
            b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n",
        )
        .map_err(|e| e.to_string())?;
    stream.flush().map_err(|e| e.to_string())
}

fn write_sse(stream: &mut std::net::TcpStream, body: &Value) -> Result<(), String> {
    let bytes = serde_json::to_vec(body).map_err(|e| e.to_string())?;
    stream.write_all(b"data: ").map_err(|e| e.to_string())?;
    stream.write_all(&bytes).map_err(|e| e.to_string())?;
    stream.write_all(b"\n\n").map_err(|e| e.to_string())?;
    stream.flush().map_err(|e| e.to_string())
}

fn write_sse_done(stream: &mut std::net::TcpStream) -> Result<(), String> {
    stream
        .write_all(b"data: [DONE]\n\n")
        .map_err(|e| e.to_string())?;
    stream.flush().map_err(|e| e.to_string())
}

fn write_json(stream: &mut std::net::TcpStream, status: u16, body: &Value) -> Result<(), String> {
    let bytes = serde_json::to_vec(body).map_err(|e| e.to_string())?;
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        500 => "Internal Server Error",
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{ChatTurn, InferenceRequest};

    fn payload(messages: serde_json::Value) -> Value {
        json!({ "model": "m.gguf", "messages": messages })
    }

    #[test]
    fn parse_trace_path_accepts_only_session_traces() {
        assert_eq!(parse_trace_path("/v1/agent/sessions/42/trace"), Some(42));
        assert_eq!(parse_trace_path("/v1/agent/sessions/0/trace"), Some(0));
        assert_eq!(parse_trace_path("/v1/agent/sessions/42"), None);
        assert_eq!(parse_trace_path("/v1/agent/sessions/abc/trace"), None);
        assert_eq!(parse_trace_path("/v1/models"), None);
    }

    #[test]
    fn chat_turns_flattens_string_and_blocks() {
        let p = payload(json!([
            { "role": "system", "content": "be terse" },
            { "role": "user", "content": "hi" },
            {
                "role": "user",
                "content": [
                    { "type": "image_url", "image_url": { "url": "data:image/png;base64,AAA" } },
                    { "type": "text", "text": "describe this" }
                ]
            },
            { "role": "user", "content": "" }
        ]));
        let turns = chat_turns(&p);
        assert_eq!(turns.len(), 3, "empty content is dropped");
        assert_eq!(
            turns[0],
            ChatTurn {
                role: "system".into(),
                content: "be terse".into()
            }
        );
        assert_eq!(
            turns[1],
            ChatTurn {
                role: "user".into(),
                content: "hi".into()
            }
        );
        assert_eq!(
            turns[2].content, "describe this",
            "array blocks joined by newline"
        );
        assert_eq!(turns[2].role, "user");
    }

    #[test]
    fn chat_request_maps_openai_params() {
        let p = json!({
            "messages": [{ "role": "user", "content": "hello" }],
            "max_completion_tokens": 4096,
            "stop": ["<|end|>", "###"]
        });
        let req: InferenceRequest = chat_request(&p).unwrap();
        let msgs = req.messages.as_ref().unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(req.max_tokens, 4096, "max_completion_tokens read");
        assert_eq!(
            req.stop_words.as_deref(),
            Some(&["<|end|>".to_string(), "###".to_string()][..])
        );

        assert!(
            chat_request(&payload(json!([]))).is_err(),
            "empty messages rejected"
        );
    }

    #[test]
    fn completions_request_uses_prompt() {
        let p = json!({ "prompt": "2+2 =", "max_tokens": 64 });
        let req: InferenceRequest = completions_request(&p).unwrap();
        assert_eq!(req.prompt, "2+2 =");
        assert_eq!(req.max_tokens, 64);
        assert!(req.messages.is_none());

        assert!(completions_request(&json!({ "prompt": "  " })).is_err());
    }

    #[test]
    fn chat_prompt_flattens_roles() {
        let p = json!({
            "messages": [
                { "role": "system", "content": "rules" },
                { "role": "user", "content": "hi" },
                { "role": "assistant", "content": "yo" }
            ]
        });
        let flat = chat_prompt(&p);
        assert!(flat.contains("rules"));
        assert!(flat.contains("User: hi"));
        assert!(flat.contains("Assistant: yo"));
        assert!(
            flat.trim_end().ends_with("Assistant:"),
            "leaves assistant tag open"
        );
    }

    #[test]
    fn responses_input_accepts_string_object_and_blocks() {
        let from_string = responses_to_chat(&json!({ "input": "hello" })).unwrap();
        assert_eq!(chat_turns(&from_string)[0].content, "hello");

        let from_object = responses_to_chat(&json!({
            "input": { "role": "user", "content": [{ "type": "input_text", "text": "blocky" }] }
        }))
        .unwrap();
        assert_eq!(chat_turns(&from_object)[0].content, "blocky");

        let from_array = responses_to_chat(&json!({
            "instructions": "be terse",
            "input": [
                { "role": "user", "content": "hi" },
                { "role": "assistant", "content": "yo" },
                { "role": "user", "content": [{ "type": "input_text", "text": "again" }] }
            ]
        }))
        .unwrap();
        let turns = chat_turns(&from_array);
        assert_eq!(turns.len(), 4, "3 input turns + leading instructions turn");
        assert_eq!(turns[0].role, "system");
        assert_eq!(turns[1].role, "user");
        assert_eq!(turns[2].role, "assistant");
        assert_eq!(turns[3].content, "again");

        assert!(
            responses_to_chat(&json!({ "input": "  " })).is_err(),
            "blank input rejected"
        );
    }

    #[test]
    fn responses_skips_tool_protocol_items() {
        // The local model has no tool channel, so function-call plumbing must
        // not be replayed as conversation text.
        let chat = responses_to_chat(&json!({
            "input": [
                { "type": "function_call", "name": "ls", "arguments": "{}" },
                { "type": "reasoning", "summary": [] },
                { "role": "user", "content": "real question" }
            ]
        }))
        .unwrap();
        let turns = chat_turns(&chat);
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].content, "real question");
    }

    #[test]
    fn responses_maps_token_and_model_fields() {
        let chat = responses_to_chat(&json!({
            "model": "local-gguf",
            "input": "hi",
            "max_output_tokens": 512
        }))
        .unwrap();
        assert_eq!(chat["model"], json!("local-gguf"));
        let req: InferenceRequest = chat_request(&chat).unwrap();
        assert_eq!(req.max_tokens, 512);
    }

    #[test]
    fn responses_body_uses_responses_envelope() {
        let done = crate::engine::InferenceDone {
            total_tokens: 30,
            generated_chars: 5,
            tokens_per_sec: 10.0,
            elapsed_ms: 1,
            stop_reason: "stop".into(),
            outcome: "completed".into(),
            input_tokens: 20,
            output_tokens: 10,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            reasoning_tokens: 0,
        };
        let body = responses_body("m", "hello", &done);
        assert_eq!(body["object"], json!("response"));
        assert_eq!(body["status"], json!("completed"));
        assert_eq!(
            body["output"][0]["content"][0]["type"],
            json!("output_text")
        );
        assert_eq!(body["output"][0]["content"][0]["text"], json!("hello"));
        assert_eq!(body["usage"]["input_tokens"], json!(20));
        assert_eq!(body["usage"]["output_tokens"], json!(10));
    }
}
