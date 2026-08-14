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

use std::time::{Duration, Instant};

use crossbeam_channel::Sender;
use futures_util::StreamExt;
use reqwest::{Client, RequestBuilder};
use serde::Deserialize;
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
#[derive(Debug, Clone, Deserialize)]
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

        let (full_text, total_tokens, stop_reason) = rt.block_on(async {
            match self.provider.style() {
                GenerationStyle::ChatCompletions => {
                    self.stream_chat(request, session_id, interrupt, tx).await
                }
                GenerationStyle::AnthropicMessages => {
                    self.stream_anthropic(request, session_id, interrupt, tx).await
                }
            }
        })?;

        let elapsed_ms = started.elapsed().as_millis() as u64;
        let tokens_per_sec = if elapsed_ms > 0 {
            total_tokens as f64 / (elapsed_ms as f64 / 1000.0)
        } else {
            0.0
        };
        Ok(GenerationOutcome {
            done: InferenceDone {
                total_tokens,
                generated_chars: full_text.chars().count() as u64,
                tokens_per_sec,
                elapsed_ms,
                stop_reason,
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
    ) -> Result<(String, u64, String), String> {
        let url = format!("{}/chat/completions", self.base_url);
        let mut body = json!({
            "model": self.model,
            "messages": [{ "role": "user", "content": request.prompt }],
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

        let resp = self
            .client
            .post(&url)
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("Remote request failed: {e}"))?;
        let resp = check_response(resp).await?;

        let mut full = String::with_capacity(1024);
        let mut total_tokens = 0u64;
        let mut stream = resp.bytes_stream();
        let mut stop_reason = "stop".to_string();

        loop {
            if interrupt.is_cancelled() {
                stop_reason = "cancelled".to_string();
                break;
            }
            let chunk = tokio::select! {
                c = stream.next() => c,
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
                    return Ok((full, total_tokens, stop_reason));
                }
                let v: Value = serde_json::from_str(data)
                    .map_err(|e| format!("Bad SSE payload: {e}"))?;

                if let Some(used) = v
                    .get("usage")
                    .and_then(|u| u.get("completion_tokens"))
                    .and_then(|t| t.as_u64())
                {
                    total_tokens = used;
                }
                let delta = v["choices"][0]["delta"]["content"]
                    .as_str()
                    .or_else(|| v["choices"][0]["text"].as_str())
                    .or_else(|| v["content_block_delta"]["delta"]["text"].as_str())
                    .unwrap_or("");
                if delta.is_empty() {
                    continue;
                }
                total_tokens += 1;
                full.push_str(delta);
                tx.send(WorkerEvent::Token { session_id, delta: delta.to_string() })
                    .map_err(|e| format!("Token stream channel closed: {e}"))?;
            }
        }

        Ok((full, total_tokens, stop_reason))
    }

    /// Anthropic Messages API streaming: `POST {base}/messages`.
    async fn stream_anthropic(
        &self,
        request: &InferenceRequest,
        session_id: u64,
        interrupt: &CancellationToken,
        tx: &Sender<WorkerEvent>,
    ) -> Result<(String, u64, String), String> {
        let url = format!("{}/messages", self.base_url);
        let mut body = json!({
            "model": self.model,
            "max_tokens": request.max_tokens,
            "temperature": request.temperature.unwrap_or(0.8),
            "messages": [{ "role": "user", "content": request.prompt }],
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

        let resp = self
            .client
            .post(&url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("Remote request failed: {e}"))?;
        let resp = check_response(resp).await?;

        let mut full = String::with_capacity(1024);
        let mut total_tokens = 0u64;
        let mut stream = resp.bytes_stream();
        let mut stop_reason = "stop".to_string();

        loop {
            if interrupt.is_cancelled() {
                stop_reason = "cancelled".to_string();
                break;
            }
            let chunk = tokio::select! {
                c = stream.next() => c,
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
                    return Ok((full, total_tokens, stop_reason));
                }
                let v: Value = serde_json::from_str(data)
                    .map_err(|e| format!("Bad SSE payload: {e}"))?;

                // Anthropic reports usage in `message_delta`, not `usage`.
                if let Some(used) = v
                    .get("message_delta")
                    .and_then(|u| u.get("usage"))
                    .and_then(|t| t.get("output_tokens"))
                    .and_then(|t| t.as_u64())
                {
                    total_tokens = used;
                }
                match v["type"].as_str() {
                    Some("message_stop") => return Ok((full, total_tokens, stop_reason)),
                    _ => {}
                }
                let delta = v["content_block_delta"]["delta"]["text"]
                    .as_str()
                    .or_else(|| v["delta"]["text"].as_str())
                    .unwrap_or("");
                if delta.is_empty() {
                    continue;
                }
                total_tokens += 1;
                full.push_str(delta);
                tx.send(WorkerEvent::Token { session_id, delta: delta.to_string() })
                    .map_err(|e| format!("Token stream channel closed: {e}"))?;
            }
        }

        Ok((full, total_tokens, stop_reason))
    }
}
