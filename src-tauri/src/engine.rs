//! Standalone llama.cpp inference engine, fully isolated from the UI thread.
//!
//! This module owns the loaded GGUF model + context (`StandaloneEngine`) and
//! exposes the entry points used by the command layer:
//!
//! * [`load_model_with_progress`] - blocking model load (run inside
//!   `spawn_blocking`), which shares one loaded model across the contexts of an
//!   [`EnginePool`].
//! * [`run_generation`] - the token loop, executed on a dedicated native worker
//!   thread that holds a context for the whole generation, streaming decoded
//!   tokens out through a bounded cross-beam MPSC channel. The UI thread never
//!   touches llama.cpp.

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
    _backend: Arc<LlamaBackend>,
}

// Safety: all llama.cpp calls are serialized by the owning async mutex and the
// borrowed model is kept alive by the `Arc` stored next to the context.
unsafe impl Send for StandaloneEngine {}

impl StandaloneEngine {
    /// Cheap metadata snapshot. Never touches the engine lock on the caller's
    /// thread; it is read while the caller already holds the lock.
    pub fn info(&self) -> ModelInfo {
        model_info(&self.model, self.context.n_ctx())
    }
}

fn model_info(model: &LlamaModel, n_ctx: u32) -> ModelInfo {
    let name = model
        .meta_val_str("general.name")
        .unwrap_or_else(|_| "unknown".to_string());
    let architecture = model
        .meta_val_str("general.architecture")
        .unwrap_or_default();
    ModelInfo {
        name,
        architecture,
        n_vocab: model.n_vocab(),
        n_ctx_train: model.n_ctx_train(),
        n_embd: model.n_embd(),
        n_layer: model.n_layer(),
        n_params: model.n_params(),
        size_bytes: model.size(),
        context_size: n_ctx,
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
    /// How many parallel engine workers (each with its own context) to spawn.
    /// Defaults to 2; threads are split across workers so the machine is not
    /// oversubscribed.
    pub n_workers: Option<u32>,
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
///
/// Token accounting mirrors the API conventions of the providers we speak to:
/// * `input_tokens` — the prompt that was sent (and filled the KV cache, since
///   every generation starts from a clean cache),
/// * `output_tokens` — the tokens the model produced,
/// * `cache_read_tokens` — tokens served from an existing prompt cache (0 for
///   local llama.cpp, which clears the KV cache every run),
/// * `cache_write_tokens` — tokens written into the prompt cache,
/// * `reasoning_tokens` — thinking tokens the model emitted (remote providers
///   that report them; 0 otherwise).
///
/// `outcome` classifies the turn lifecycle: `"completed" | "failed" |
/// "interrupted" | "error"` (see the blueprint's `finished` event).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InferenceDone {
    pub total_tokens: u64,
    pub generated_chars: u64,
    pub tokens_per_sec: f64,
    pub elapsed_ms: u64,
    pub stop_reason: String,
    /// Turn lifecycle outcome: "completed" | "failed" | "interrupted" | "error".
    pub outcome: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub reasoning_tokens: u64,
}

/// Per-step telemetry for agentic tasks (camelCase).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StepStat {
    pub step: usize,
    /// Phase/group this step belongs to, e.g. "Plan", "Execute" or
    /// "Subtask 1/3 · Fix lint" — lets the frontend render a grouped timeline.
    pub group: String,
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

// ---------------------------------------------------------------------------
// Engine pool: one worker thread per generator, no `'static` transmute.
//
// The old design transmuted a `&mut Box<dyn TextGenerator>` to `'static` and
// handed it to a fresh `std::thread::spawn` per generation. The pool instead
// owns every generator inside a dedicated worker thread for its whole life;
// callers hold cheap cloneable [`PoolGenerator`] handles that proxy requests
// over a channel and block on a reply. That removes the unsoundness *and* lets
// several generations run truly concurrently (one per worker), which is what
// parallel sub-tasks need.
// ---------------------------------------------------------------------------

/// One message to an engine worker thread.
pub enum EngineMsg {
    Generate {
        request: InferenceRequest,
        session_id: u64,
        interrupt: CancellationToken,
        reply: crossbeam_channel::Sender<Result<GenerationOutcome, String>>,
    },
    Stop,
}

/// A handle to one worker in an [`EnginePool`]. Cheap to clone (shares the
/// worker's channel); implements [`TextGenerator`] by message-passing, so the
/// orchestrator can hold one handle per worker and drive concurrent subtasks.
#[derive(Clone)]
pub struct PoolGenerator {
    tx: crossbeam_channel::Sender<EngineMsg>,
    info: ModelInfo,
}

impl PoolGenerator {
    fn new(tx: crossbeam_channel::Sender<EngineMsg>, info: ModelInfo) -> Self {
        Self { tx, info }
    }
}

impl TextGenerator for PoolGenerator {
    fn info(&self) -> ModelInfo {
        self.info.clone()
    }

    fn generate(
        &mut self,
        request: &InferenceRequest,
        session_id: u64,
        interrupt: &CancellationToken,
        _tx: &Sender<WorkerEvent>,
    ) -> Result<GenerationOutcome, String> {
        let (reply_tx, reply_rx) =
            crossbeam_channel::bounded::<Result<GenerationOutcome, String>>(1);
        self.tx
            .send(EngineMsg::Generate {
                request: request.clone(),
                session_id,
                interrupt: interrupt.clone(),
                reply: reply_tx,
            })
            .map_err(|_| "Engine worker is gone".to_string())?;
        reply_rx
            .recv()
            .map_err(|_| "Engine worker is gone".to_string())?
    }
}

struct EngineWorker {
    tx: crossbeam_channel::Sender<EngineMsg>,
    info: ModelInfo,
    _handle: std::thread::JoinHandle<()>,
}

/// A pool of engine worker threads, each owning its own generator (own
/// llama.cpp context for local models, own client for remote). Generations
/// dispatch to workers round-robin; parallel subtasks take one worker each.
pub struct EnginePool {
    workers: Vec<EngineWorker>,
}

impl EnginePool {
    /// Spawn `count` worker threads from a factory that builds one generator
    /// per worker. `event_tx` is the shared channel that token/step events
    /// stream through to the UI.
    pub fn spawn_with<F>(
        mut factory: F,
        event_tx: Sender<WorkerEvent>,
        count: usize,
    ) -> Result<Self, String>
    where
        F: FnMut() -> Result<Box<dyn TextGenerator>, String>,
    {
        let mut workers = Vec::with_capacity(count.max(1));
        for _ in 0..count.max(1) {
            let inner = factory()?;
            let info = inner.info();
            let (tx, rx) = crossbeam_channel::unbounded::<EngineMsg>();
            let event_tx = event_tx.clone();
            let handle = std::thread::spawn(move || {
                let mut inner = inner;
                for msg in rx.iter() {
                    match msg {
                        EngineMsg::Generate {
                            request,
                            session_id,
                            interrupt,
                            reply,
                        } => {
                            let result =
                                inner.generate(&request, session_id, &interrupt, &event_tx);
                            let _ = reply.send(result);
                        }
                        EngineMsg::Stop => break,
                    }
                }
            });
            workers.push(EngineWorker {
                tx,
                info,
                _handle: handle,
            });
        }
        Ok(Self { workers })
    }

    pub fn len(&self) -> usize {
        self.workers.len()
    }

    pub fn info(&self) -> ModelInfo {
        self.workers
            .first()
            .map(|w| w.info.clone())
            .unwrap_or_else(|| ModelInfo {
                name: "unloaded".into(),
                architecture: String::new(),
                n_vocab: 0,
                n_ctx_train: 0,
                n_embd: 0,
                n_layer: 0,
                n_params: 0,
                size_bytes: 0,
                context_size: 4096,
            })
    }

    /// Round-robin handle to worker `idx % len`.
    pub fn handle(&self, idx: usize) -> PoolGenerator {
        let w = &self.workers[idx % self.workers.len()];
        PoolGenerator::new(w.tx.clone(), w.info.clone())
    }
}

impl Drop for EnginePool {
    /// Signal every worker to shut down and join it, so llama.cpp contexts are
    /// released before the model is dropped.
    fn drop(&mut self) {
        for w in &self.workers {
            let _ = w.tx.send(EngineMsg::Stop);
        }
        let workers = std::mem::take(&mut self.workers);
        for w in workers {
            let _ = w._handle.join();
        }
    }
}

/// A loaded GGUF model (backend + weights), shared across several contexts.
/// The expensive part of a load (mmap + tensor deserialization) happens once;
/// each [`LoadedModel::new_engine_with_threads`] then creates a fresh context
/// cheaply, so a parallel engine pool can share one model load.
pub struct LoadedModel {
    backend: Arc<LlamaBackend>,
    model: Arc<LlamaModel>,
    n_ctx: u32,
}

impl LoadedModel {
    /// Create a new standalone engine (own context) using a specific thread
    /// count (used to split compute across pool workers).
    pub fn new_engine_with_threads(&self, threads: i32) -> Result<StandaloneEngine, String> {
        let ctx_params = LlamaContextParams::default()
            .with_n_ctx(NonZeroU32::new(self.n_ctx))
            .with_n_threads(threads)
            .with_n_threads_batch(threads)
            .with_n_batch(512);

        let context = self
            .model
            .new_context(&self.backend, ctx_params)
            .map_err(|e| format!("Failed to create the model context: {e}"))?;

        // SAFETY: `model` and `backend` are pinned in `Arc`s stored in the
        // same struct (backend declared after context, so freed after it) and
        // are dropped after `context` (field order), so the erased borrows
        // never dangle.
        let context: LlamaContext<'static> = unsafe { std::mem::transmute(context) };

        Ok(StandaloneEngine {
            context,
            model: self.model.clone(),
            _backend: self.backend.clone(),
        })
    }
}

/// AppHandle-free model load used by tests and headless tooling. `on_progress`
/// receives llama.cpp's load progress (0.0 → 1.0) and may cancel by returning
/// `false`.
pub fn load_model_with_progress<F>(
    path: &Path,
    params: &ModelInitParams,
    mut on_progress: F,
) -> Result<LoadedModel, String>
where
    F: FnMut(f32) -> bool + Send + 'static,
{
    if !path.is_file() {
        return Err(format!("Model file does not exist: {}", path.display()));
    }

    let backend = Arc::new(
        LlamaBackend::init()
            .map_err(|e| format!("Failed to initialize the llama backend: {e}"))?,
    );

    let n_gpu_layers = params.n_gpu_layers.unwrap_or(0);
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

    Ok(LoadedModel {
        backend,
        model: Arc::new(model),
        n_ctx,
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

    // Local llama.cpp clears the KV cache before every run, so the prompt was
    // fully written into cache (write = input) and nothing was served from a
    // previous run's cache (read = 0). No reasoning tokens are reported.
    Ok(GenerationOutcome {
        done: InferenceDone {
            total_tokens,
            generated_chars,
            tokens_per_sec,
            elapsed_ms: elapsed.as_millis() as u64,
            stop_reason: stop_reason.to_string(),
            outcome: if stop_reason == "cancelled" {
                "interrupted".to_string()
            } else {
                "completed".to_string()
            },
            input_tokens: prompt_len as u64,
            output_tokens: total_tokens,
            cache_read_tokens: 0,
            cache_write_tokens: prompt_len as u64,
            reasoning_tokens: 0,
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

    /// Deterministic fake generator so the pool can be tested without a model.
    struct FakeGen {
        tag: String,
    }

    impl TextGenerator for FakeGen {
        fn info(&self) -> ModelInfo {
            ModelInfo {
                name: self.tag.clone(),
                architecture: "fake".into(),
                n_vocab: 0,
                n_ctx_train: 0,
                n_embd: 0,
                n_layer: 0,
                n_params: 0,
                size_bytes: 0,
                context_size: 2048,
            }
        }

        fn generate(
            &mut self,
            request: &InferenceRequest,
            session_id: u64,
            _interrupt: &CancellationToken,
            tx: &Sender<WorkerEvent>,
        ) -> Result<GenerationOutcome, String> {
            let _ = tx.send(WorkerEvent::Token {
                session_id,
                delta: self.tag.clone(),
            });
            Ok(GenerationOutcome {
                done: InferenceDone {
                    total_tokens: request.max_tokens as u64,
                    generated_chars: request.max_tokens as u64,
                    tokens_per_sec: 0.0,
                    elapsed_ms: 1,
                    stop_reason: "done".into(),
                    outcome: "completed".into(),
                    input_tokens: 1,
                    output_tokens: request.max_tokens as u64,
                    cache_read_tokens: 0,
                    cache_write_tokens: 1,
                    reasoning_tokens: 0,
                },
                full_text: self.tag.clone(),
            })
        }
    }

    #[test]
    fn pool_runs_generations_on_separate_workers_in_parallel() {
        let (ev_tx, _ev_rx) = bounded::<WorkerEvent>(256);
        let pool = EnginePool::spawn_with(
            || {
                Ok(Box::new(FakeGen { tag: "worker".into() }) as Box<dyn TextGenerator>)
            },
            ev_tx.clone(),
            2,
        )
        .expect("spawn pool");
        assert_eq!(pool.len(), 2);
        assert_eq!(pool.info().name, "worker");

        let request = InferenceRequest {
            prompt: "p".into(),
            max_tokens: 8,
            temperature: None,
            top_p: None,
            seed: None,
            stop_words: None,
        };
        let interrupt = CancellationToken::new();

        let mut g1 = pool.handle(0);
        let mut g2 = pool.handle(1);
        let ev1 = ev_tx.clone();
        let ev2 = ev_tx.clone();
        let req1 = request.clone();
        let req2 = request.clone();
        let i1 = interrupt.clone();
        let i2 = interrupt.clone();

        let h1 = std::thread::spawn(move || g1.generate(&req1, 1, &i1, &ev1));
        let h2 = std::thread::spawn(move || g2.generate(&req2, 2, &i2, &ev2));

        let o1 = h1.join().expect("worker 1 thread").expect("generation 1 ok");
        let o2 = h2.join().expect("worker 2 thread").expect("generation 2 ok");
        assert_eq!(o1.full_text, "worker");
        assert_eq!(o2.full_text, "worker");
        assert_eq!(o1.done.total_tokens, 8);
        assert_eq!(o2.done.total_tokens, 8);
    }

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
            n_workers: None,
        };
        let model = load_model_with_progress(&path, &params, |_| true).expect("load model");
        let mut engine = model.new_engine_with_threads(4).expect("build engine");

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
