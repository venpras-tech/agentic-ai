//! Remote backends for the model router, modelled after Roo Code's provider
//! system: a registry of well-known providers (each with a default base URL,
//! auth style and wire protocol) plus a free-form "Custom" OpenAI-compatible
//! entry. The UI fetches the provider's available models through
//! [`list_models`] and streams generation through `POST {base}/chat/completions`
//! (OpenAI-compatible) or the Anthropic Messages API.
//!
//! The agent protocol stays identical across backends: the orchestrator passes
//! the assembled prompt as a single user message and parses `<execute_tool>`
//! blocks from the streamed reply, so swapping providers never touches the loop.
//!
//! The API key lives in memory only — it is never written to disk. Streaming
//! races the circuit breaker; an abort drops the HTTP connection immediately.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crossbeam_channel::Sender;
use futures_util::StreamExt;
use reqwest::{Client, RequestBuilder};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use crate::engine::{
    GenerationOutcome, InferenceDone, InferenceRequest, ModelInfo, TextGenerator, WorkerEvent,
};

/// Providers known to the frontend preset registry. `Custom` is the escape
/// hatch for any OpenAI-compatible endpoint (vLLM, LiteLLM, gateways, …).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteProvider {
    OpenAI,
    Anthropic,
    OpenRouter,
    Google,
    Ollama,
    LmStudio,
    DeepSeek,
    Xai,
    Groq,
    Mistral,
    Custom,
}

/// Which wire protocol `generate` speaks for a provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GenerationStyle {
    /// `POST {base}/chat/completions` (SSE), Bearer auth.
    ChatCompletions,
    /// `POST {base}/messages` (SSE), `x-api-key` + version headers.
    AnthropicMessages,
}

impl RemoteProvider {
    pub fn from_str(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "openai" => Self::OpenAI,
            "anthropic" => Self::Anthropic,
            "openrouter" => Self::OpenRouter,
            "google" => Self::Google,
            "ollama" => Self::Ollama,
            "lmstudio" => Self::LmStudio,
            "deepseek" => Self::DeepSeek,
            "xai" => Self::Xai,
            "groq" => Self::Groq,
            "mistral" => Self::Mistral,
            _ => Self::Custom,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::OpenAI => "openai",
            Self::Anthropic => "anthropic",
            Self::OpenRouter => "openrouter",
            Self::Google => "google",
            Self::Ollama => "ollama",
            Self::LmStudio => "lmstudio",
            Self::DeepSeek => "deepseek",
            Self::Xai => "xai",
            Self::Groq => "groq",
            Self::Mistral => "mistral",
            Self::Custom => "custom",
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::OpenAI => "OpenAI",
            Self::Anthropic => "Anthropic (Claude)",
            Self::OpenRouter => "OpenRouter",
            Self::Google => "Google Gemini",
            Self::Ollama => "Ollama (local)",
            Self::LmStudio => "LM Studio (local)",
            Self::DeepSeek => "DeepSeek",
            Self::Xai => "xAI (Grok)",
            Self::Groq => "Groq",
            Self::Mistral => "Mistral",
            Self::Custom => "Custom (OpenAI-compatible)",
        }
    }

    /// Local servers don't need a key; everything else does.
    pub fn requires_api_key(&self) -> bool {
        !matches!(self, Self::Ollama | Self::LmStudio)
    }

    pub fn style(&self) -> GenerationStyle {
        match self {
            Self::Anthropic => GenerationStyle::AnthropicMessages,
            _ => GenerationStyle::ChatCompletions,
        }
    }
}

fn default_provider() -> String {
    RemoteProvider::Custom.as_str().into()
}

/// User-supplied remote endpoint settings (camelCase over the wire).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteModelConfig {
    /// Provider id from the preset registry, e.g. `openai`, `ollama`, `custom`.
    #[serde(default = "default_provider")]
    pub provider: String,
    /// e.g. `https://api.openai.com/v1` or `http://localhost:11434/v1`.
    pub base_url: String,
    /// Bearer / `x-api-key` token. Memory-only; never persisted.
    pub api_key: String,
    /// Model identifier, e.g. `gpt-4o-mini`, `qwen3:8b`.
    pub model: String,
    /// Assumed context budget for the eviction engine (default 128k).
    #[serde(default)]
    pub context_size: Option<u32>,
}

/// Time budget for a single request (long agent steps can stream for minutes).
const REQUEST_TIMEOUT_SECS: u64 = 600;
/// Time budget for the lightweight model-list probe.
const MODELS_TIMEOUT_SECS: u64 = 30;
/// Cap on the error body echoed back to the UI.
const MAX_ERROR_BODY: usize = 500;
/// Stall watchdog: if the provider stops sending bytes for this long mid-stream
/// (no chunk at all), we assume the connection hung and fail the generation so
/// the agent loop can react instead of blocking forever.
const STALL_TIMEOUT_SECS: u64 = 90;
/// How many times a transient request failure (connect error, 408/429/5xx) is
/// retried before giving up. Retries only happen *before* streaming begins;
/// a failure mid-stream never re-sends, which would duplicate output.
const MAX_RETRIES: u32 = 2;

/// Provider-reported (or estimated) token usage for one remote generation.
#[derive(Default)]
struct RemoteUsage {
    input_tokens: u64,
    output_tokens: u64,
    cache_read_tokens: u64,
    cache_write_tokens: u64,
    reasoning_tokens: u64,
}

/// Exponential backoff for retry `attempt` (1st retry = 1s, 2nd = 2s).
fn backoff(attempt: u32) -> Duration {
    Duration::from_millis(500 * (1u64 << attempt))
}

/// Statuses worth retrying before the stream starts. 429 (rate limit) and 5xx
/// (upstream hiccup) are the classic transient cases; 408 is a stalled gateway.
fn retriable(status: reqwest::StatusCode) -> bool {
    status == reqwest::StatusCode::TOO_MANY_REQUESTS
        || status.as_u16() == 408
        || status.is_server_error()
}

/// Send a (already built, cloneable) request with bounded retries and backoff.
/// Non-retriable failures still run through [`check_response`] for a readable
/// error body.
async fn send_with_retry(builder: RequestBuilder) -> Result<reqwest::Response, String> {
    let mut attempt = 0u32;
    loop {
        let req = builder
            .try_clone()
            .ok_or_else(|| "HTTP request builder cannot be reused".to_string())?;
        match req.send().await {
            Err(_e) if attempt < MAX_RETRIES => {
                attempt += 1;
                tokio::time::sleep(backoff(attempt)).await;
            }
            Err(e) => return Err(format!("Remote request failed: {e}")),
            Ok(resp) if resp.status().is_success() => return Ok(resp),
            Ok(resp) if attempt < MAX_RETRIES && retriable(resp.status()) => {
                attempt += 1;
                tokio::time::sleep(backoff(attempt)).await;
            }
            Ok(resp) => return check_response(resp).await,
        }
    }
}

/// Parse an OpenAI-compatible `usage` object (chat/completions). Falls back to
/// the locally counted values when the provider omits them.
fn usage_from_chat(v: &Value, est_input: u64, est_output: u64) -> RemoteUsage {
    let usage = v.get("usage").unwrap_or(&Value::Null);
    let input = usage
        .get("prompt_tokens")
        .and_then(|t| t.as_u64())
        .unwrap_or(est_input);
    let output = usage
        .get("completion_tokens")
        .and_then(|t| t.as_u64())
        .unwrap_or(est_output);
    let cache_read = usage
        .pointer("/prompt_tokens_details/cached_tokens")
        .and_then(|t| t.as_u64())
        .unwrap_or(0);
    let reasoning = usage
        .pointer("/completion_tokens_details/reasoning_tokens")
        .and_then(|t| t.as_u64())
        .unwrap_or(0);
    RemoteUsage {
        input_tokens: input,
        output_tokens: output,
        cache_read_tokens: cache_read.min(input),
        cache_write_tokens: input.saturating_sub(cache_read),
        reasoning_tokens: reasoning,
    }
}

/// Parse an Anthropic Messages `usage` object (both the `message_start` usage
/// for input/cache and the `message_delta` usage for output).
fn usage_from_anthropic(v: &Value, est_input: u64, est_output: u64) -> RemoteUsage {
    let msg_usage = v
        .get("message")
        .and_then(|m| m.get("usage"))
        .unwrap_or(&Value::Null);
    let input = msg_usage
        .get("input_tokens")
        .and_then(|t| t.as_u64())
        .unwrap_or(est_input);
    let cache_read = msg_usage
        .get("cache_read_input_tokens")
        .and_then(|t| t.as_u64())
        .unwrap_or(0);
    let cache_write = msg_usage
        .get("cache_creation_input_tokens")
        .and_then(|t| t.as_u64())
        .unwrap_or(input.saturating_sub(cache_read));
    let output = v
        .pointer("/message_delta/usage/output_tokens")
        .and_then(|t| t.as_u64())
        .unwrap_or(est_output);
    let reasoning = msg_usage
        .get("reasoning_input_tokens")
        .and_then(|t| t.as_u64())
        .unwrap_or(0);
    RemoteUsage {
        input_tokens: input,
        output_tokens: output,
        cache_read_tokens: cache_read,
        cache_write_tokens: cache_write,
        reasoning_tokens: reasoning,
    }
}

/// Cheap prompt-size estimate (chars/4) used when a provider omits usage.
fn est_input_tokens(request: &InferenceRequest) -> u64 {
    (request.prompt.chars().count() / 4).max(1) as u64
}

/// Fetch the provider's available model ids so the UI can offer them as a
/// dropdown. Endpoint and auth are chosen per provider (Roo Code style):
/// OpenAI-compatible servers answer `GET {base}/models`; Ollama exposes
/// `/api/tags` on its origin (stripping the `/v1` OpenAI shim); Anthropic
/// requires `x-api-key` + `anthropic-version` headers.
pub async fn list_models(config: &RemoteModelConfig) -> Result<Vec<String>, String> {
    let provider = RemoteProvider::from_str(&config.provider);
    let base_url = config.base_url.trim().trim_end_matches('/').to_string();
    if base_url.is_empty() {
        return Err("Enter a base URL first".into());
    }

    let client = Client::builder()
        .timeout(Duration::from_secs(MODELS_TIMEOUT_SECS))
        .build()
        .map_err(|e| format!("Failed to build HTTP client: {e}"))?;

    let req: RequestBuilder = match provider {
        RemoteProvider::Anthropic => {
            let url = format!("{base_url}/models");
            client
                .get(&url)
                .header("x-api-key", config.api_key.trim())
                .header("anthropic-version", "2023-06-01")
        }
        RemoteProvider::Ollama => {
            let origin = base_url.strip_suffix("/v1").unwrap_or(&base_url);
            let url = format!("{origin}/api/tags");
            client.get(url)
        }
        _ => {
            let url = format!("{base_url}/models");
            let mut req = client.get(&url);
            if !config.api_key.trim().is_empty() {
                req = req.bearer_auth(config.api_key.trim());
            }
            req
        }
    };

    let resp = req
        .send()
        .await
        .map_err(|e| format!("Model list request failed: {e}"))?;
    let resp = check_response(resp).await?;
    let v: Value = resp
        .json()
        .await
        .map_err(|e| format!("Bad model list payload: {e}"))?;

    let mut ids: Vec<String> = match provider {
        RemoteProvider::Ollama => v["models"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|m| m["name"].as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default(),
        _ => v["data"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|m| m["id"].as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default(),
    };

    // Ollama treats `foo:latest` as `foo`; don't show two identical entries.
    if provider == RemoteProvider::Ollama {
        for id in ids.iter_mut() {
            if let Some(stripped) = id.strip_suffix(":latest") {
                *id = stripped.to_string();
            }
        }
    }
    ids.sort();
    ids.dedup();

    if ids.is_empty() {
        return Err("The provider returned no models".into());
    }
    Ok(ids)
}

async fn check_response(resp: reqwest::Response) -> Result<reqwest::Response, String> {
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        let truncated: String = text.chars().take(MAX_ERROR_BODY).collect();
        return Err(format!("Remote API error {status}: {truncated}"));
    }
    Ok(resp)
}

/// Split a `data:image/png;base64,AAAA...` URL into `(media_type, base64_body)`.
/// Returns `None` when the string is not a base64 data URL.
fn split_data_url(data_url: &str) -> Option<(String, String)> {
    if !data_url.starts_with("data:") {
        return None;
    }
    let rest = &data_url["data:".len()..];
    let (meta, body) = rest.split_once(',')?;
    if !meta.contains(";base64") {
        return None;
    }
    Some((
        meta.split(';').next().unwrap_or("image/png").to_string(),
        body.to_string(),
    ))
}

/// Build the `content` field of the leading user message. With no images this is
/// the plain prompt string (unchanged from today). With images it becomes an
/// array of multimodal content blocks — OpenAI schema for
/// [`GenerationStyle::ChatCompletions`], Anthropic schema otherwise.
fn user_content_value(request: &InferenceRequest, anthropic: bool) -> Value {
    let images = request.images.as_deref().unwrap_or_default();
    if images.is_empty() {
        return Value::String(request.prompt.clone());
    }
    let mut blocks: Vec<Value> = Vec::with_capacity(images.len() + 1);
    if anthropic {
        blocks.push(json!({ "type": "text", "text": request.prompt }));
        for img in images {
            if let Some((media, data)) = split_data_url(&img.data_url) {
                blocks.push(json!({
                    "type": "image",
                    "source": { "type": "base64", "media_type": media, "data": data }
                }));
            }
        }
    } else {
        for img in images {
            blocks.push(json!({
                "type": "image_url",
                "image_url": { "url": img.data_url }
            }));
        }
        blocks.push(json!({ "type": "text", "text": request.prompt }));
    }
    Value::Array(blocks)
}

pub struct RemoteGenerator {
    client: Client,
    base_url: String,
    api_key: String,
    model: String,
    provider: RemoteProvider,
    info: ModelInfo,
}

impl RemoteGenerator {
    pub fn new(config: RemoteModelConfig) -> Result<Self, String> {
        let provider = RemoteProvider::from_str(&config.provider);
        let base_url = config.base_url.trim().trim_end_matches('/').to_string();
        if base_url.is_empty() {
            return Err("Remote model needs a base URL".into());
        }
        if config.model.trim().is_empty() {
            return Err("Remote model needs a model name".into());
        }
        if config.api_key.trim().is_empty() && provider.requires_api_key() {
            return Err(format!("{} needs an API key", provider.label()));
        }
        let client = Client::builder()
            .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS))
            .build()
            .map_err(|e| format!("Failed to build HTTP client: {e}"))?;
        let context_size = config.context_size.unwrap_or(128_000);
        Ok(Self {
            client,
            base_url,
            api_key: config.api_key,
            model: config.model.clone(),
            provider,
            info: ModelInfo {
                name: config.model,
                architecture: "remote-api".into(),
                n_vocab: 0,
                n_ctx_train: 0,
                n_embd: 0,
                n_layer: 0,
                n_params: 0,
                size_bytes: 0,
                context_size,
            },
        })
    }
}

impl TextGenerator for RemoteGenerator {
    fn info(&self) -> ModelInfo {
        self.info.clone()
    }

    fn generate(
        &mut self,
        request: &InferenceRequest,
        session_id: u64,
        interrupt: &CancellationToken,
        tx: &Sender<WorkerEvent>,
    ) -> Result<GenerationOutcome, String> {
        let started = Instant::now();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| format!("Failed to start remote runtime: {e}"))?;

        let (full_text, usage, stop_reason) = rt.block_on(async {
            match self.provider.style() {
                GenerationStyle::ChatCompletions => {
                    self.stream_chat(request, session_id, interrupt, tx).await
                }
                GenerationStyle::AnthropicMessages => {
                    self.stream_anthropic(request, session_id, interrupt, tx)
                        .await
                }
            }
        })?;

        let elapsed_ms = started.elapsed().as_millis() as u64;
        let output_tokens = usage.output_tokens;
        let tokens_per_sec = if elapsed_ms > 0 {
            output_tokens as f64 / (elapsed_ms as f64 / 1000.0)
        } else {
            0.0
        };
        let outcome = if stop_reason == "cancelled" {
            "interrupted".to_string()
        } else {
            "completed".to_string()
        };
        Ok(GenerationOutcome {
            done: InferenceDone {
                total_tokens: output_tokens,
                generated_chars: full_text.chars().count() as u64,
                tokens_per_sec,
                elapsed_ms,
                stop_reason,
                outcome,
                input_tokens: usage.input_tokens,
                output_tokens,
                cache_read_tokens: usage.cache_read_tokens,
                cache_write_tokens: usage.cache_write_tokens,
                reasoning_tokens: usage.reasoning_tokens,
            },
            full_text,
        })
    }
}

impl RemoteGenerator {
    /// OpenAI-compatible streaming: `POST {base}/chat/completions`.
    async fn stream_chat(
        &self,
        request: &InferenceRequest,
        session_id: u64,
        interrupt: &CancellationToken,
        tx: &Sender<WorkerEvent>,
    ) -> Result<(String, RemoteUsage, String), String> {
        let url = format!("{}/chat/completions", self.base_url);
        let user_content = user_content_value(request, false);
        let mut body = json!({
            "model": self.model,
            "messages": [{ "role": "user", "content": user_content }],
            "stream": true,
            "max_tokens": request.max_tokens,
            "temperature": request.temperature.unwrap_or(0.8),
            "top_p": request.top_p.unwrap_or(0.95),
        });
        let stop = request.stop_words.clone().unwrap_or_default();
        let stop: Vec<Value> = stop
            .into_iter()
            .filter(|s| !s.is_empty())
            .map(Value::String)
            .collect();
        if !stop.is_empty() {
            body["stop"] = Value::Array(stop);
        }

        let resp = send_with_retry(
            self.client
                .post(&url)
                .bearer_auth(&self.api_key)
                .json(&body),
        )
        .await?;

        let est_input = est_input_tokens(request);
        let mut full = String::with_capacity(1024);
        let mut counted_output = 0u64;
        let mut usage = RemoteUsage {
            input_tokens: est_input,
            ..RemoteUsage::default()
        };
        let mut stream = resp.bytes_stream();
        let mut stop_reason = "stop".to_string();

        loop {
            if interrupt.is_cancelled() {
                stop_reason = "cancelled".to_string();
                break;
            }
            // Stall watchdog: a fresh timeout per chunk, so a provider that
            // keeps streaming but slowly is fine; one that goes silent for
            // STALL_TIMEOUT_SECS is treated as hung.
            let chunk = tokio::select! {
                c = stream.next() => c,
                _ = tokio::time::sleep(Duration::from_secs(STALL_TIMEOUT_SECS)) => {
                    return Err(format!(
                        "Remote stream stalled: no data received for {STALL_TIMEOUT_SECS}s. \
                         The provider may be hung; cancelling."
                    ));
                }
                _ = interrupt.clone().cancelled_owned() => {
                    stop_reason = "cancelled".to_string();
                    break;
                }
            };
            let Some(bytes) = chunk else {
                break; // stream closed by server
            };
            let bytes = bytes.map_err(|e| format!("Remote stream error: {e}"))?;
            let text = String::from_utf8_lossy(&bytes);

            for raw in text.lines() {
                let line = raw.trim();
                if !line.starts_with("data:") {
                    continue;
                }
                let data = line["data:".len()..].trim();
                if data == "[DONE]" {
                    return Ok((full, usage, stop_reason));
                }
                let v: Value =
                    serde_json::from_str(data).map_err(|e| format!("Bad SSE payload: {e}"))?;

                if v.get("usage").is_some() {
                    usage = usage_from_chat(&v, est_input, counted_output);
                }
                let delta = v["choices"][0]["delta"]["content"]
                    .as_str()
                    .or_else(|| v["choices"][0]["text"].as_str())
                    .or_else(|| v["content_block_delta"]["delta"]["text"].as_str())
                    .unwrap_or("");
                if delta.is_empty() {
                    continue;
                }
                counted_output += 1;
                full.push_str(delta);
                tx.send(WorkerEvent::Token {
                    session_id,
                    delta: delta.to_string(),
                })
                .map_err(|e| format!("Token stream channel closed: {e}"))?;
            }
        }

        usage.output_tokens = usage.output_tokens.max(counted_output);
        Ok((full, usage, stop_reason))
    }

    /// Anthropic Messages API streaming: `POST {base}/messages`.
    async fn stream_anthropic(
        &self,
        request: &InferenceRequest,
        session_id: u64,
        interrupt: &CancellationToken,
        tx: &Sender<WorkerEvent>,
    ) -> Result<(String, RemoteUsage, String), String> {
        let url = format!("{}/messages", self.base_url);
        let user_content = user_content_value(request, true);
        let mut body = json!({
            "model": self.model,
            "max_tokens": request.max_tokens,
            "temperature": request.temperature.unwrap_or(0.8),
            "messages": [{ "role": "user", "content": user_content }],
            "stream": true,
        });
        let stop = request.stop_words.clone().unwrap_or_default();
        let stop: Vec<Value> = stop
            .into_iter()
            .filter(|s| !s.is_empty())
            .map(Value::String)
            .collect();
        if !stop.is_empty() {
            body["stop_sequences"] = Value::Array(stop);
        }

        let resp = send_with_retry(
            self.client
                .post(&url)
                .header("x-api-key", &self.api_key)
                .header("anthropic-version", "2023-06-01")
                .json(&body),
        )
        .await?;

        let est_input = est_input_tokens(request);
        let mut full = String::with_capacity(1024);
        let mut counted_output = 0u64;
        let mut usage = RemoteUsage {
            input_tokens: est_input,
            ..RemoteUsage::default()
        };
        let mut stream = resp.bytes_stream();
        let mut stop_reason = "stop".to_string();

        loop {
            if interrupt.is_cancelled() {
                stop_reason = "cancelled".to_string();
                break;
            }
            // Stall watchdog — same contract as the chat-completions path.
            let chunk = tokio::select! {
                c = stream.next() => c,
                _ = tokio::time::sleep(Duration::from_secs(STALL_TIMEOUT_SECS)) => {
                    return Err(format!(
                        "Remote stream stalled: no data received for {STALL_TIMEOUT_SECS}s. \
                         The provider may be hung; cancelling."
                    ));
                }
                _ = interrupt.clone().cancelled_owned() => {
                    stop_reason = "cancelled".to_string();
                    break;
                }
            };
            let Some(bytes) = chunk else {
                break; // stream closed by server
            };
            let bytes = bytes.map_err(|e| format!("Remote stream error: {e}"))?;
            let text = String::from_utf8_lossy(&bytes);

            for raw in text.lines() {
                let line = raw.trim();
                if !line.starts_with("data:") {
                    continue;
                }
                let data = line["data:".len()..].trim();
                if data == "[DONE]" {
                    return Ok((full, usage, stop_reason));
                }
                let v: Value =
                    serde_json::from_str(data).map_err(|e| format!("Bad SSE payload: {e}"))?;

                // Anthropic reports usage in two places: `message_start` carries
                // input/cache tokens, `message_delta` carries the running output
                // count. `usage_from_anthropic` reads both.
                match v["type"].as_str() {
                    Some("message_start") | Some("message_delta") => {
                        usage = usage_from_anthropic(&v, est_input, counted_output);
                    }
                    Some("message_stop") => return Ok((full, usage, stop_reason)),
                    _ => {}
                }
                let delta = v["content_block_delta"]["delta"]["text"]
                    .as_str()
                    .or_else(|| v["delta"]["text"].as_str())
                    .unwrap_or("");
                if delta.is_empty() {
                    continue;
                }
                counted_output += 1;
                full.push_str(delta);
                tx.send(WorkerEvent::Token {
                    session_id,
                    delta: delta.to_string(),
                })
                .map_err(|e| format!("Token stream channel closed: {e}"))?;
            }
        }

        usage.output_tokens = usage.output_tokens.max(counted_output);
        Ok((full, usage, stop_reason))
    }
}

// ---------------------------------------------------------------------------
// Multi-provider registry & role-based routing
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ProviderRole {
    Planner,
    Editor,
    Autocomplete,
    Embed,
}

impl ProviderRole {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Planner => "planner",
            Self::Editor => "editor",
            Self::Autocomplete => "autocomplete",
            Self::Embed => "embed",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "editor" => Self::Editor,
            "autocomplete" => Self::Autocomplete,
            "embed" => Self::Embed,
            _ => Self::Planner,
        }
    }
}

impl std::fmt::Display for ProviderRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ProviderKind {
    Local,
    OpenAI,
    Ollama,
    OpenRouter,
    Anthropic,
    Google,
    LmStudio,
    DeepSeek,
    Xai,
    Groq,
    Mistral,
    Custom,
}

impl ProviderKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::OpenAI => "openai",
            Self::Ollama => "ollama",
            Self::OpenRouter => "openrouter",
            Self::Anthropic => "anthropic",
            Self::Google => "google",
            Self::LmStudio => "lmstudio",
            Self::DeepSeek => "deepseek",
            Self::Xai => "xai",
            Self::Groq => "groq",
            Self::Mistral => "mistral",
            Self::Custom => "custom",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "local" => Self::Local,
            "openai" => Self::OpenAI,
            "ollama" => Self::Ollama,
            "openrouter" => Self::OpenRouter,
            "anthropic" => Self::Anthropic,
            "google" => Self::Google,
            "lmstudio" => Self::LmStudio,
            "deepseek" => Self::DeepSeek,
            "xai" => Self::Xai,
            "groq" => Self::Groq,
            "mistral" => Self::Mistral,
            _ => Self::Custom,
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::Local => "Local (GGUF)",
            Self::OpenAI => "OpenAI",
            Self::Ollama => "Ollama",
            Self::OpenRouter => "OpenRouter",
            Self::Anthropic => "Anthropic (Claude)",
            Self::Google => "Google Gemini",
            Self::LmStudio => "LM Studio",
            Self::DeepSeek => "DeepSeek",
            Self::Xai => "xAI (Grok)",
            Self::Groq => "Groq",
            Self::Mistral => "Mistral",
            Self::Custom => "Custom (OpenAI-compatible)",
        }
    }

    pub fn requires_api_key(&self) -> bool {
        !matches!(self, Self::Local | Self::Ollama | Self::LmStudio)
    }

    pub fn generation_style(&self) -> GenerationStyle {
        match self {
            Self::Anthropic => GenerationStyle::AnthropicMessages,
            _ => GenerationStyle::ChatCompletions,
        }
    }
}

impl std::fmt::Display for ProviderKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A single provider entry in the registry: identity, connection details, and
/// which roles it serves. `Local` providers need no base_url/api_key.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderConfig {
    pub id: String,
    pub name: String,
    pub kind: ProviderKind,
    /// `None` for Local providers.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// `None` for providers that don't need a key (Local, Ollama, LmStudio).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    /// Model identifier, e.g. `gpt-4o-mini`, `qwen3:8b`.
    pub model: String,
    #[serde(default)]
    pub roles: Vec<ProviderRole>,
    /// Assumed context size for eviction; defaults to 128k when absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_size: Option<u32>,
}

impl ProviderConfig {
    pub fn local(model: impl Into<String>) -> Self {
        Self {
            id: "local".into(),
            name: "Local GGUF".into(),
            kind: ProviderKind::Local,
            base_url: None,
            api_key: None,
            model: model.into(),
            roles: vec![ProviderRole::Planner, ProviderRole::Editor],
            context_size: None,
        }
    }

    pub fn remote(
        kind: ProviderKind,
        name: impl Into<String>,
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        model: impl Into<String>,
    ) -> Self {
        Self {
            id: kind.as_str().into(),
            name: name.into(),
            kind,
            base_url: Some(base_url.into()),
            api_key: Some(api_key.into()),
            model: model.into(),
            roles: Vec::new(),
            context_size: None,
        }
    }

    /// Build a [`RemoteModelConfig`] suitable for passing to
    /// [`RemoteGenerator::new`]. Returns `Err` for Local providers.
    pub fn to_remote_model_config(&self) -> Result<RemoteModelConfig, String> {
        if self.kind == ProviderKind::Local {
            return Err("Local providers cannot produce a RemoteModelConfig".into());
        }
        let base_url = self
            .base_url
            .as_deref()
            .unwrap_or("")
            .to_string();
        if base_url.is_empty() {
            return Err(format!("Provider '{}' has no base_url", self.id));
        }
        Ok(RemoteModelConfig {
            provider: self.kind.as_str().into(),
            base_url,
            api_key: self.api_key.clone().unwrap_or_default(),
            model: self.model.clone(),
            context_size: self.context_size,
        })
    }
}

/// A registry of known providers and a role → provider mapping.
///
/// Default construction mirrors today's single-local + optional-remote setup so
/// `main.rs` can adopt the registry incrementally without breaking. Cloning
/// snapshots the current role map + provider table for hand-off to worker
/// threads (runtime role routing).
#[derive(Clone)]
pub struct ProviderRegistry {
    providers: Vec<ProviderConfig>,
    role_map: HashMap<ProviderRole, String>,
}

impl ProviderRegistry {
    /// Empty registry (no providers).
    pub fn new() -> Self {
        Self {
            providers: Vec::new(),
            role_map: HashMap::new(),
        }
    }

    /// Create a registry from a single local + optional remote, matching the
    /// current single-provider behaviour exactly.
    pub fn with_defaults(
        local_model: &str,
        remote: Option<RemoteModelConfig>,
    ) -> Self {
        let mut reg = Self::new();
        let mut local = ProviderConfig::local(local_model);
        local.roles = vec![ProviderRole::Planner, ProviderRole::Editor];
        reg.add_provider(local);
        if let Some(rc) = remote {
            let kind = ProviderKind::from_str(&rc.provider);
            let remote_cfg = ProviderConfig::remote(
                kind,
                kind.label(),
                &rc.base_url,
                &rc.api_key,
                &rc.model,
            );
            reg.add_provider(remote_cfg);
        }
        reg
    }

    pub fn add_provider(&mut self, config: ProviderConfig) {
        if let Some(existing) = self.providers.iter_mut().find(|p| p.id == config.id) {
            existing.roles = config.roles.clone();
            existing.base_url = config.base_url.clone();
            existing.api_key = config.api_key.clone();
            existing.model = config.model.clone();
            existing.name = config.name.clone();
            existing.kind = config.kind;
            existing.context_size = config.context_size;
        } else {
            self.providers.push(config);
        }
    }

    pub fn remove_provider(&mut self, id: &str) -> bool {
        let before = self.providers.len();
        self.providers.retain(|p| p.id != id);
        if self.providers.len() < before {
            self.role_map.retain(|_, pid| pid != id);
            true
        } else {
            false
        }
    }

    pub fn set_role_provider(&mut self, role: ProviderRole, provider_id: impl Into<String>) {
        self.role_map.insert(role, provider_id.into());
    }

    pub fn clear_role(&mut self, role: ProviderRole) {
        self.role_map.remove(&role);
    }

    pub fn providers(&self) -> &[ProviderConfig] {
        &self.providers
    }

    pub fn get_provider(&self, id: &str) -> Option<&ProviderConfig> {
        self.providers.iter().find(|p| p.id == id)
    }

    /// Return the provider assigned to `role`. Falls back in order:
    /// 1. Explicit role → provider mapping
    /// 2. First provider that lists `role` in its `roles` vec
    /// 3. First provider in the list (convention: local)
    pub fn route(&self, role: ProviderRole) -> Option<&ProviderConfig> {
        if let Some(pid) = self.role_map.get(&role) {
            if let Some(p) = self.providers.iter().find(|p| &p.id == pid) {
                return Some(p);
            }
        }
        if let Some(p) = self.providers.iter().find(|p| p.roles.contains(&role)) {
            return Some(p);
        }
        self.providers.first()
    }

    /// Find a non-Local provider whose `model`, `id` or `name` matches `needle`
    /// (case-insensitive). This is what lets a subagent `modelOverride` route
    /// to a distinct remote generator at runtime; unknown or local names return
    /// `None` so the caller falls back to the pooled local worker.
    pub fn find_best_remote_provider(&self, needle: &str) -> Option<&ProviderConfig> {
        let needle = needle.trim().to_lowercase();
        if needle.is_empty() {
            return None;
        }
        self.providers.iter().find(|p| {
            p.kind != ProviderKind::Local
                && (p.model.to_lowercase() == needle
                    || p.id.to_lowercase() == needle
                    || p.name.to_lowercase() == needle)
        })
    }
}

impl Default for ProviderRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod registry_tests {
    use super::*;

    #[test]
    fn default_route_falls_back_to_first_provider() {
        let reg = ProviderRegistry::with_defaults("qwen-0.5b", None);
        let p = reg.route(ProviderRole::Planner).expect("should route");
        assert_eq!(p.kind, ProviderKind::Local);
        assert_eq!(p.model, "qwen-0.5b");
    }

    #[test]
    fn explicit_role_mapping_takes_priority() {
        let mut reg = ProviderRegistry::with_defaults("qwen-0.5b", None);
        let remote = ProviderConfig::remote(
            ProviderKind::OpenAI,
            "OpenAI",
            "https://api.openai.com/v1",
            "sk-test",
            "gpt-4o-mini",
        );
        let remote_id = remote.id.clone();
        reg.add_provider(remote);
        reg.set_role_provider(ProviderRole::Editor, &remote_id);

        let editor = reg.route(ProviderRole::Editor).expect("editor routed");
        assert_eq!(editor.kind, ProviderKind::OpenAI);
        assert_eq!(editor.model, "gpt-4o-mini");

        let planner = reg.route(ProviderRole::Planner).expect("planner routed");
        assert_eq!(planner.kind, ProviderKind::Local);
    }

    #[test]
    fn role_in_config_roles_is_used_when_no_explicit_mapping() {
        let mut reg = ProviderRegistry::new();
        let mut local = ProviderConfig::local("mymodel");
        local.roles = vec![ProviderRole::Planner];
        reg.add_provider(local);

        let mut remote = ProviderConfig::remote(
            ProviderKind::OpenRouter,
            "OpenRouter",
            "https://openrouter.ai/api/v1",
            "sk-or-xxx",
            "anthropic/claude-sonnet",
        );
        remote.roles = vec![ProviderRole::Editor, ProviderRole::Autocomplete];
        reg.add_provider(remote);

        let planner = reg.route(ProviderRole::Planner).expect("planner");
        assert_eq!(planner.id, "local");

        let editor = reg.route(ProviderRole::Editor).expect("editor");
        assert_eq!(editor.id, "openrouter");

        let autocomplete = reg.route(ProviderRole::Autocomplete).expect("ac");
        assert_eq!(autocomplete.id, "openrouter");

        let embed = reg.route(ProviderRole::Embed).expect("embed fallback");
        assert_eq!(embed.id, "local");
    }

    #[test]
    fn add_and_remove_provider() {
        let mut reg = ProviderRegistry::with_defaults("qwen", None);
        assert_eq!(reg.providers().len(), 1);

        let remote = ProviderConfig::remote(
            ProviderKind::Ollama,
            "Ollama",
            "http://localhost:11434/v1",
            "",
            "llama3:8b",
        );
        reg.add_provider(remote);
        assert_eq!(reg.providers().len(), 2);

        assert!(reg.remove_provider("ollama"));
        assert_eq!(reg.providers().len(), 1);
        assert!(!reg.remove_provider("nonexistent"));
    }

    #[test]
    #[test]
    fn find_best_remote_provider_matches_model_id_or_name_case_insensitive() {
        let mut reg = ProviderRegistry::new();
        reg.add_provider(ProviderConfig::local("qwen-0.5b"));
        let remote = ProviderConfig::remote(
            ProviderKind::OpenRouter,
            "OpenRouter",
            "https://openrouter.ai/api/v1",
            "sk-or-xxx",
            "anthropic/claude-sonnet",
        );
        let id = remote.id.clone();
        reg.add_provider(remote);

        // Match by model name (any case).
        let by_model = reg.find_best_remote_provider("Anthropic/Claude-Sonnet");
        assert!(by_model.is_some());
        assert_eq!(by_model.unwrap().kind, ProviderKind::OpenRouter);
        // Match by provider id.
        let by_id = reg.find_best_remote_provider(&id);
        assert!(by_id.is_some());
        // Match by display name.
        let by_name = reg.find_best_remote_provider("openrouter");
        assert!(by_name.is_some());
        // Unknown override -> None (caller falls back to the local pool).
        assert!(reg.find_best_remote_provider("llama4:70b").is_none());
        // Local model name is never treated as a remote match.
        assert!(reg.find_best_remote_provider("qwen-0.5b").is_none());
        // Empty/whitespace -> None.
        assert!(reg.find_best_remote_provider("   ").is_none());
    }

    fn to_remote_model_config_round_trips_correctly() {
        let cfg = ProviderConfig::remote(
            ProviderKind::OpenAI,
            "OpenAI",
            "https://api.openai.com/v1",
            "sk-abc",
            "gpt-4o",
        );
        let rmc = cfg.to_remote_model_config().expect("conversion");
        assert_eq!(rmc.base_url, "https://api.openai.com/v1");
        assert_eq!(rmc.api_key, "sk-abc");
        assert_eq!(rmc.model, "gpt-4o");
        assert_eq!(rmc.provider, "openai");
    }

    #[test]
    fn local_provider_cannot_become_remote_model_config() {
        let cfg = ProviderConfig::local("model.gguf");
        assert!(cfg.to_remote_model_config().is_err());
    }

    #[test]
    fn empty_registry_returns_none() {
        let reg = ProviderRegistry::new();
        assert!(reg.route(ProviderRole::Planner).is_none());
    }

    #[test]
    fn provider_kind_from_str_and_as_str_roundtrip() {
        for kind in [
            ProviderKind::Local,
            ProviderKind::OpenAI,
            ProviderKind::Ollama,
            ProviderKind::OpenRouter,
            ProviderKind::Anthropic,
            ProviderKind::Google,
            ProviderKind::LmStudio,
            ProviderKind::DeepSeek,
            ProviderKind::Xai,
            ProviderKind::Groq,
            ProviderKind::Mistral,
            ProviderKind::Custom,
        ] {
            assert_eq!(ProviderKind::from_str(kind.as_str()), kind);
        }
    }

    #[test]
    fn provider_role_from_str_and_as_str_roundtrip() {
        for role in [
            ProviderRole::Planner,
            ProviderRole::Editor,
            ProviderRole::Autocomplete,
            ProviderRole::Embed,
        ] {
            assert_eq!(ProviderRole::from_str(role.as_str()), role);
        }
    }
}
