//! Standalone llama.cpp inference engine, fully isolated from the UI thread.
//!
//! This module owns the loaded GGUF model + context (`StandaloneEngine`) and
//! exposes two entry points used by the command layer:
//!
//! * [`load_engine`] - blocking model load (run inside `spawn_blocking`), with
//!   progress pushed to the frontend through Tauri's emitter.
//! * [`run_generation`] - the token loop. It is executed on a dedicated native
//!   OS thread that holds the engine's mutex via `blocking_lock` for the whole
//!   generation, streaming decoded tokens out through a bounded cross-beam MPSC
//!   channel. The UI thread never touches llama.cpp.

use std::num::NonZeroU32;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use crossbeam_channel::Sender;
use encoding_rs::UTF_8;
use llama_cpp_2::context::params::LlamaContextParams;
use llama_cpp_2::context::LlamaContext;
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::model::{AddBos, LlamaModel};
use llama_cpp_2::sampling::LlamaSampler;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};
use tokio_util::sync::CancellationToken;

/// A loaded GGUF model, its context and the owned llama backend.
///
/// # `Send` safety
/// `LlamaContext` wraps raw llama.cpp pointers and is not `Send` by itself; the
/// crate upstream already marks `LlamaModel` as `Send + Sync`. We confine the
/// context to one thread at a time through the surrounding `tokio::sync::Mutex`
/// (the worker acquires it with `blocking_lock` for the whole generation), so
/// moving the wrapper between threads is sound.
///
/// # Drop order
/// Fields are declared so that the context is freed first, then the model, then
/// the backend (llama.cpp requires contexts to outlive their model).
pub struct StandaloneEngine {
    context: LlamaContext<'static>,
    model: Arc<LlamaModel>,
    _backend: LlamaBackend,
}

// Safety: all llama.cpp calls are serialized by the owning async mutex and the
// borrowed model is kept alive by the `Arc` stored next to the context.
unsafe impl Send for StandaloneEngine {}

impl StandaloneEngine {
    /// Cheap metadata snapshot. Never touches the engine lock on the caller's
    /// thread; it is read while the caller already holds the lock.
    pub fn info(&self) -> ModelInfo {
        let name = self
            .model
            .meta_val_str("general.name")
            .unwrap_or_else(|_| "unknown".to_string());
        let architecture = self
            .model
            .meta_val_str("general.architecture")
            .unwrap_or_default();
        ModelInfo {
            name,
            architecture,
            n_vocab: self.model.n_vocab(),
            n_ctx_train: self.model.n_ctx_train(),
            n_embd: self.model.n_embd(),
            n_layer: self.model.n_layer(),
            n_params: self.model.n_params(),
            size_bytes: self.model.size(),
            context_size: self.context.n_ctx(),
        }
    }
}

/// User-supplied model loading parameters (camelCase over the wire).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelInitParams {
    /// Layers offloaded to the GPU (0 = CPU only).
    pub n_gpu_layers: Option<u32>,
    /// Requested context (KV cache) size in tokens.
    pub context_size: Option<u32>,
    /// Compute threads; defaults to all available cores.
    pub n_threads: Option<u32>,
}

/// A single streaming inference request (camelCase over the wire).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InferenceRequest {
    pub prompt: String,
    pub max_tokens: u32,
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
    pub seed: Option<u32>,
    pub stop_words: Option<Vec<String>>,
}

/// Snapshot of model metadata surfaced to the frontend (camelCase).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelInfo {
    pub name: String,
    pub architecture: String,
    pub n_vocab: i32,
    pub n_ctx_train: u32,
    pub n_embd: i32,
    pub n_layer: u32,
    pub n_params: u64,
    pub size_bytes: u64,
    pub context_size: u32,
}

/// Terminal statistics for a finished generation (camelCase).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InferenceDone {
    pub total_tokens: u64,
    pub generated_chars: u64,
    pub tokens_per_sec: f64,
    pub elapsed_ms: u64,
    pub stop_reason: String,
}

/// Per-step telemetry for agentic tasks (camelCase).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StepStat {
    pub step: usize,
    pub tokens: u64,
    pub elapsed_ms: u64,
    pub tool_calls: usize,
}

/// Sub-task progress for decomposed agentic tasks (camelCase). `status` is one
/// of "running" | "done" | "failed".
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SubtaskStat {
    pub index: usize,
    pub total: usize,
    pub title: String,
    pub status: String,
}

/// Model load progress pushed to the frontend (camelCase).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoadProgressEvent {
    pub stage: &'static str,
    pub progress: f32,
}

/// Result of a generation that the caller may need beyond the wire stats: the
/// full generated text (used by the orchestrator to parse tool calls).
pub struct GenerationOutcome {
    pub done: InferenceDone,
    pub full_text: String,
}

/// Messages flowing from the native worker thread back to the async emitter.
/// Every variant carries the `session_id` so multi-session rendering (plain
/// chat *and* multi-step agent tasks) always keys events to the right stream.
pub enum WorkerEvent {
    /// A decoded slice of the generated text.
    Token { session_id: u64, delta: String },
    /// A completed orchestrator step (agentic tasks only).
    Step { session_id: u64, step: StepStat },
    /// A sub-task in a decomposed task changed state (agentic tasks only).
    Subtask { session_id: u64, subtask: SubtaskStat },
    /// Terminal success: generation statistics.
    Done { session_id: u64, done: InferenceDone },
    /// Terminal failure: a typed string error.
    Error { session_id: u64, message: String },
}

/// The model-router abstraction: a uniform "give me a completion" contract
/// that local llama.cpp (`StandaloneEngine`) and a remote OpenAI-compatible
/// API (`RemoteGenerator`) both implement. The orchestrator and the streaming
/// chat command only ever speak to `Box<dyn TextGenerator>`.
pub trait TextGenerator: Send {
    /// Metadata snapshot for the status bar (name, context size, …).
    fn info(&self) -> ModelInfo;
    /// Run one generation, streaming decoded tokens through `tx`. Identical
    /// contract to [`run_generation`], so both callers behave the same
    /// regardless of backend.
    fn generate(
        &mut self,
        request: &InferenceRequest,
        session_id: u64,
        interrupt: &CancellationToken,
        tx: &Sender<WorkerEvent>,
    ) -> Result<GenerationOutcome, String>;
}

/// Local llama.cpp backend: thin wrapper over [`StandaloneEngine`].
pub struct LocalGenerator {
    engine: StandaloneEngine,
}

impl LocalGenerator {
    pub fn new(engine: StandaloneEngine) -> Self {
        Self { engine }
    }
}

impl TextGenerator for LocalGenerator {
    fn info(&self) -> ModelInfo {
        self.engine.info()
    }

    fn generate(
        &mut self,
        request: &InferenceRequest,
        session_id: u64,
        interrupt: &CancellationToken,
        tx: &Sender<WorkerEvent>,
    ) -> Result<GenerationOutcome, String> {
        run_generation(&mut self.engine, request, session_id, interrupt, tx)
    }
}

/// Blocking model load. Call from `tokio::task::spawn_blocking` so the UI
/// thread never stalls on mmap + tensor deserialization.
pub fn load_engine(path: &Path, params: &ModelInitParams, app: &AppHandle) -> Result<StandaloneEngine, String> {
    let app_progress = app.clone();
    let mut last_progress = 0.0f32;
    load_engine_with_progress(path, params, move |progress: f32| {
        // Throttle the event stream to avoid flooding the webview.
        if progress - last_progress >= 0.01 || progress >= 1.0 {
            last_progress = progress;
            let _ = app_progress.emit_to(
                "main",
                "model-load-progress",
                LoadProgressEvent {
                    stage: "load",
                    progress,
                },
            );
        }
        true
    })
}

/// AppHandle-free model load used by tests and headless tooling. `on_progress`
/// receives llama.cpp's load progress (0.0 → 1.0) and may cancel by returning
/// `false`.
pub fn load_engine_with_progress<F>(
    path: &Path,
    params: &ModelInitParams,
    mut on_progress: F,
) -> Result<StandaloneEngine, String>
where
    F: FnMut(f32) -> bool + Send + 'static,
{
    if !path.is_file() {
        return Err(format!("Model file does not exist: {}", path.display()));
    }

    let backend = LlamaBackend::init()
        .map_err(|e| format!("Failed to initialize the llama backend: {e}"))?;

    let n_gpu_layers = params.n_gpu_layers.unwrap_or(0);
    let n_threads = params
        .n_threads
        .unwrap_or_else(|| {
            std::thread::available_parallelism()
                .map(|n| n.get() as u32)
                .unwrap_or(4)
        })
        .clamp(1, 256) as i32;
    let requested_ctx = params.context_size.unwrap_or(4096);

    let model_params = LlamaModelParams::default()
        .with_n_gpu_layers(n_gpu_layers)
        .with_use_mmap(true)
        .with_progress_callback(move |progress: f32| on_progress(progress));

    let model = LlamaModel::load_from_file(&backend, path, &model_params)
        .map_err(|e| format!("Failed to load model: {e}"))?;

    // Clamp the KV cache to what the model was trained on (never silently
    // allocate more than `n_ctx_train`).
    let n_ctx = requested_ctx.clamp(64, model.n_ctx_train().max(64));

    let ctx_params = LlamaContextParams::default()
        .with_n_ctx(NonZeroU32::new(n_ctx))
        .with_n_threads(n_threads)
        .with_n_threads_batch(n_threads)
        .with_n_batch(512);

    let model = Arc::new(model);
    let context = model
        .new_context(&backend, ctx_params)
        .map_err(|e| format!("Failed to create the model context: {e}"))?;

    // SAFETY: `model` is pinned in the `Arc` stored in the same struct and is
    // dropped after `context` (field order), so the erased borrow never dangles.
    let context: LlamaContext<'static> = unsafe { std::mem::transmute(context) };

    Ok(StandaloneEngine {
        context,
        model,
        _backend: backend,
    })
}

/// The generation loop. Runs on the dedicated worker thread.
///
/// `interrupt` is the circuit breaker's [`CancellationToken`]; it is polled
/// between tokens (a cheap atomic load) so an abort takes effect within one
/// decode step. Each decoded token is pushed through the bounded cross-beam
/// channel `tx` (tagged with `session_id`); backpressure naturally throttles
/// the worker when the emitter is slower than the GPU/CPU.
pub fn run_generation(
    engine: &mut StandaloneEngine,
    request: &InferenceRequest,
    session_id: u64,
    interrupt: &CancellationToken,
    tx: &Sender<WorkerEvent>,
) -> Result<GenerationOutcome, String> {
    let n_ctx = engine.context.n_ctx() as i32;
    let max_tokens = request.max_tokens.max(1) as i32;

    let prompt_tokens = engine
        .model
        .str_to_token(&request.prompt, AddBos::Always)
        .map_err(|e| format!("Failed to tokenize prompt: {e}"))?;
    let prompt_len = prompt_tokens.len() as i32;
    if prompt_len >= n_ctx {
        return Err(format!(
            "Prompt is {prompt_len} tokens but the context is only {n_ctx} tokens"
        ));
    }

    // Clear any KV state left over from a previous session so positions start
    // from zero and cross-request caches never bleed into one another.
    engine.context.clear_kv_cache();
    engine.context.reset_timings();

    let mut sampler = build_sampler(request);

    // The prompt can easily exceed a single LlamaBatch (capacity = n_batch,
    // 512). Chunk it so every `decode` call stays within the batch capacity;
    // logits are only requested on the final prompt token so generation starts
    // from the correct KV position.
    const PROMPT_BATCH: usize = 512;
    let mut batch = LlamaBatch::new(PROMPT_BATCH, 1);
    let mut pos = 0usize;
    while pos < prompt_tokens.len() {
        let end = (pos + PROMPT_BATCH).min(prompt_tokens.len());
        batch.clear();
        for (i, token) in prompt_tokens[pos..end].iter().enumerate() {
            let idx = pos + i;
            let is_last = idx == prompt_tokens.len() - 1;
            batch
                .add(*token, idx as i32, &[0], is_last)
                .map_err(|e| format!("Failed to queue prompt token: {e}"))?;
        }
        engine
            .context
            .decode(&mut batch)
            .map_err(|e| format!("Prompt evaluation failed: {e}"))?;
        pos = end;
    }

    let started = Instant::now();
    let mut n_cur = prompt_len;
    let mut total_tokens = 0u64;
    let mut generated_chars = 0u64;
    let mut full_text = String::with_capacity(256);
    let mut decoder = UTF_8.new_decoder();
    let stop_words = request.stop_words.clone().unwrap_or_default();

    let stop_reason = loop {
        if interrupt.is_cancelled() {
            break "cancelled";
        }
        if total_tokens as i32 >= max_tokens {
            break "max-tokens";
        }
        if n_cur >= n_ctx {
            break "context-full";
        }

        let token = sampler.sample(&engine.context, batch.n_tokens() - 1);
        sampler.accept(token);

        if engine.model.is_eog_token(token) {
            break "eos";
        }

        let piece = engine
            .model
            .token_to_piece(token, &mut decoder, false, None)
            .map_err(|e| format!("Failed to decode token: {e}"))?;

        // Some control/unknown tokens decode to an empty piece; still advance
        // the KV position so the loop cannot spin.
        if piece.is_empty() {
            batch.clear();
            batch
                .add(token, n_cur, &[0], true)
                .map_err(|e| format!("Failed to queue token: {e}"))?;
            engine
                .context
                .decode(&mut batch)
                .map_err(|e| format!("Token evaluation failed: {e}"))?;
            n_cur += 1;
            continue;
        }

        full_text.push_str(&piece);
        generated_chars += piece.chars().count() as u64;
        total_tokens += 1;

        // Stop-word check on the rolling suffix (never emits the offending piece).
        if has_stop_suffix(&full_text, &stop_words) {
            break "stop-word";
        }

        tx.send(WorkerEvent::Token { session_id, delta: piece })
            .map_err(|e| format!("Token stream channel closed: {e}"))?;

        batch.clear();
        batch
            .add(token, n_cur, &[0], true)
            .map_err(|e| format!("Failed to queue token: {e}"))?;
        engine
            .context
            .decode(&mut batch)
            .map_err(|e| format!("Token evaluation failed: {e}"))?;
        n_cur += 1;
    };

    let elapsed = started.elapsed();
    let tokens_per_sec = if elapsed.as_secs_f64() > 0.0 {
        total_tokens as f64 / elapsed.as_secs_f64()
    } else {
        0.0
    };

    Ok(GenerationOutcome {
        done: InferenceDone {
            total_tokens,
            generated_chars,
            tokens_per_sec,
            elapsed_ms: elapsed.as_millis() as u64,
            stop_reason: stop_reason.to_string(),
        },
        full_text,
    })
}

/// Build the sampler chain. Must end with a token-selecting sampler
/// (`greedy` or `dist`); temperature/`top_p` are applied beforehand.
fn build_sampler(request: &InferenceRequest) -> LlamaSampler {
    let temperature = request.temperature.unwrap_or(0.8).clamp(0.0, 4.0);
    let top_p = request.top_p.unwrap_or(0.95).clamp(0.0, 1.0);
    let seed = request.seed.unwrap_or(u32::MAX);

    let mut samplers: Vec<LlamaSampler> = Vec::with_capacity(3);
    if temperature > 0.0 {
        samplers.push(LlamaSampler::temp(temperature));
    }
    if top_p > 0.0 && top_p < 1.0 {
        samplers.push(LlamaSampler::top_p(top_p, 1));
    }
    samplers.push(if temperature <= 0.0 {
        LlamaSampler::greedy()
    } else {
        LlamaSampler::dist(seed)
    });
    LlamaSampler::chain_simple(samplers)
}

fn has_stop_suffix(text: &str, stop_words: &[String]) -> bool {
    stop_words
        .iter()
        .any(|w| !w.is_empty() && text.len() >= w.len() && text.ends_with(w.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossbeam_channel::bounded;

    /// End-to-end headless chat test: load the real GGUF, run `run_generation`
    /// (the exact path the app's chat uses, including the chunked prompt
    /// decode), and confirm tokens actually stream. Skipped when the model file
    /// is absent; run explicitly with `cargo test -- --ignored`.
    #[test]
    #[ignore = "requires the real GGUF model file on disk"]
    fn headless_chat_generation_streams_tokens() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../models/qwen2.5-0.5b-instruct-q4_k_m.gguf");
        if !path.is_file() {
            eprintln!("SKIP: model not found at {}", path.display());
            return;
        }

        let params = ModelInitParams {
            context_size: Some(2048),
            n_threads: Some(4),
            n_gpu_layers: Some(0),
        };
        let mut engine =
            load_engine_with_progress(&path, &params, |_| true).expect("load engine");

        let (tx, rx) = bounded::<WorkerEvent>(512);
        let interrupt = CancellationToken::new();
        let request = InferenceRequest {
            prompt: "hi".to_string(),
            max_tokens: 48,
            temperature: Some(0.0),
            top_p: Some(1.0),
            seed: Some(42),
            stop_words: Some(vec![]),
        };
        let outcome = run_generation(&mut engine, &request, 1, &interrupt, &tx)
            .expect("generation must not fail");

        let mut streamed = String::new();
        let mut token_events = 0usize;
        while let Ok(ev) = rx.try_recv() {
            if let WorkerEvent::Token { delta, .. } = ev {
                token_events += 1;
                streamed.push_str(&delta);
            }
        }

        eprintln!("prompt_tokens={} total_tokens={} stop_reason={}",
            request.prompt.split_whitespace().count(), outcome.done.total_tokens, outcome.done.stop_reason);
        eprintln!("token_events={token_events}");
        eprintln!("FULL_TEXT:\n{}", outcome.full_text);
        eprintln!("STREAMED OUTPUT:\n{streamed}");
        assert!(
            !streamed.trim().is_empty(),
            "no tokens streamed for prompt 'hi' — chat path broken"
        );
        assert!(outcome.done.total_tokens > 0);

        // A second turn over the same context must also work (no KV bleed).
        let request2 = InferenceRequest {
            prompt: "what is 2+2?".to_string(),
            max_tokens: 32,
            temperature: Some(0.0),
            top_p: Some(1.0),
            seed: Some(42),
            stop_words: Some(vec![]),
        };
        let _ = run_generation(&mut engine, &request2, 1, &interrupt, &tx)
            .expect("second generation must not fail");
        eprintln!("second turn OK");

        // Regression: a prompt longer than one 512-token batch must decode via
        // the chunked path (the old single-batch code failed here with
        // "Insufficient Space of 512").
        let big_prompt = format!(
            "Here is a long instruction list for you. {}\nNow answer: what is 2+2?",
            "A numbered fact: 42. ".repeat(130)
        );
        let big_request = InferenceRequest {
            prompt: big_prompt,
            max_tokens: 24,
            temperature: Some(0.0),
            top_p: Some(1.0),
            seed: Some(42),
            stop_words: Some(vec![]),
        };
        let big = run_generation(&mut engine, &big_request, 1, &interrupt, &tx)
            .expect("long-prompt generation must not fail");
        eprintln!("long prompt OK ({} tokens, {})", big.done.total_tokens, big.done.stop_reason);
        assert!(!big.full_text.trim().is_empty());
    }
}
