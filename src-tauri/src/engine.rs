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

use std::collections::HashMap;
use std::num::NonZeroU32;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use crate::logging;
use crate::remote::RemoteGenerator;
use crossbeam_channel::Sender;
use encoding_rs::UTF_8;
use llama_cpp_2::context::params::LlamaContextParams;
use llama_cpp_2::context::LlamaContext;
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::model::{AddBos, LlamaChatMessage, LlamaModel};
use llama_cpp_2::sampling::LlamaSampler;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

#[allow(unused_imports)]
pub use crate::remote::{
    ProviderConfig, ProviderKind, ProviderRegistry, ProviderRole, RemoteModelConfig,
};

/// C callback that forwards native llama.cpp / ggml log output into our
/// logging pipeline so model loads surface in the Console window and the
/// rolling `.log` file instead of vanishing into raw stderr.
extern "C" fn native_model_log(
    level: llama_cpp_sys_2::ggml_log_level,
    text: *const std::os::raw::c_char,
    _user: *mut std::ffi::c_void,
) {
    if text.is_null() {
        return;
    }
    let chunk = unsafe { std::ffi::CStr::from_ptr(text) }.to_string_lossy();
    for line in chunk.lines() {
        let line = line.trim_end();
        if line.is_empty() {
            continue;
        }
        match level {
            llama_cpp_sys_2::GGML_LOG_LEVEL_ERROR => logging::error(None, "model", line),
            llama_cpp_sys_2::GGML_LOG_LEVEL_WARN => logging::warn(None, "model", line),
            _ => logging::info(None, "model", line),
        }
    }
}

/// Route all native llama.cpp and ggml logs through [`logging`]. Call once at
/// startup, before any model load; nothing else ever overwrites the hook.
pub fn install_native_model_logs() {
    unsafe {
        llama_cpp_sys_2::llama_log_set(Some(native_model_log), std::ptr::null_mut());
        // GGML must be set after llama: setting llama resets ggml too.
        llama_cpp_sys_2::ggml_log_set(Some(native_model_log), std::ptr::null_mut());
    }
}

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
    /// The exact token IDs (prompt + generated output) currently held in the KV
    /// cache of `context`. Used to compute a *verified* common prefix before
    /// reusing the cache on the next request — mirroring how llama-server
    /// reuses a cache only up to the first divergent token. Generated output
    /// tokens that don't round-trip identically are simply not reused.
    cached_tokens: Vec<llama_cpp_2::token::LlamaToken>,
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

/// One structured conversation turn used to render the prompt through the
/// model's own chat template (see [`InferenceRequest::messages`]).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ChatTurn {
    pub role: String,
    pub content: String,
}

/// An attached image, carried as a base64 data URL so it can be injected as a
/// vision content block for multimodal remote providers. Local llama.cpp has no
/// vision, so these are ignored by the local render path and only surfaced to
/// OpenAI-compatible / Anthropic remote backends that accept image blocks.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageAttachment {
    /// `data:image/png;base64,...` (or similar). The media type is parsed from
    /// the data URL; the payload is the base64 body.
    pub data_url: String,
    /// Optional short label / description the model can reference.
    #[serde(default)]
    pub alt: String,
}

/// A single streaming inference request (camelCase over the wire).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InferenceRequest {
    /// Flat fallback prompt. Used verbatim when [`Self::messages`] is absent
    /// or the model ships no chat template.
    pub prompt: String,
    /// Structured conversation turns. When present AND the loaded GGUF embeds
    /// a chat template, they are rendered through it (`add_ass = true`) so the
    /// model receives the exact instruction format it was tuned with — this is
    /// what keeps the app "in sync" across ChatML / Llama-3 / Mistral / …
    /// models. Roles other than `user`/`assistant` are passed as `system`.
    #[serde(default)]
    pub messages: Option<Vec<ChatTurn>>,
    /// Base64 image attachments to include with the user turn for vision-capable
    /// remote providers. Ignored by the local llama.cpp path.
    #[serde(default)]
    pub images: Option<Vec<ImageAttachment>>,
    pub max_tokens: u32,
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
    /// Repetition-penalty multiplier applied over the last few tokens.
    /// `None` / <=1.0 disables it; values >1.0 suppress tokens that already
    /// appeared recently, which stops small models from degenerately looping
    /// (echoing prompt fragments forever). Defaults to [`REPEAT_PENALTY_DEFAULT`].
    pub repeat_penalty: Option<f32>,
    pub seed: Option<u32>,
    pub stop_words: Option<Vec<String>>,
    /// Number of prompt tokens already present in the KV cache from a previous
    /// generation call on the same worker. When set, the engine skips clearing
    /// the cache and only processes tokens from this offset onward, saving
    /// prompt re-evaluation cost on multi-step agent loops.
    #[serde(default)]
    pub cached_prefix_tokens: Option<usize>,
}

/// Default repetition penalty when a request does not specify one.
pub const REPEAT_PENALTY_DEFAULT: f32 = 1.15;
/// How many recent tokens the repetition penalty looks back at.
const REPEAT_LAST_N: i32 = 64;

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
///
/// The three optional fields feed the row-by-row subagent status panel: the
/// model the sub-task ran on, how long it has been running, and the tool it is
/// currently executing (`None` while generating / between tools).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SubtaskStat {
    pub index: usize,
    pub total: usize,
    pub title: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default)]
    pub elapsed_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
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
    Subtask {
        session_id: u64,
        subtask: SubtaskStat,
    },
    /// Terminal success: generation statistics.
    Done {
        session_id: u64,
        done: InferenceDone,
    },
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

// ---------------------------------------------------------------------------
// Runtime role routing (model hand-off).
//
// The [`EnginePool`] is built from one backend (usually local GGUF). The
// [`RuntimeRouter`] lets the orchestrator hand a generation off to a different
// provider mid-task by role: e.g. a cheap/fast remote model plans while the
// pool's flagship model edits. Local routes (the common case) borrow a worker
// handle from the pool; non-local routes lazily spawn a [`RemoteGenerator`]
// and cache one per role. The router is single-threaded by design (it lives on
// the orchestrator's own thread), so no interior mutability is needed.
// ---------------------------------------------------------------------------

/// A generator produced by [`RuntimeRouter::resolve`] for one routed step.
pub enum RoutedGen<'a> {
    /// A handle into the primary engine pool (local provider).
    Pool(PoolGenerator),
    /// A lazily-created remote generator cached for the role.
    Remote(&'a mut dyn TextGenerator),
}

impl TextGenerator for RoutedGen<'_> {
    fn info(&self) -> ModelInfo {
        match self {
            RoutedGen::Pool(g) => g.info(),
            RoutedGen::Remote(g) => g.info(),
        }
    }

    fn generate(
        &mut self,
        request: &InferenceRequest,
        session_id: u64,
        interrupt: &CancellationToken,
        tx: &Sender<WorkerEvent>,
    ) -> Result<GenerationOutcome, String> {
        match self {
            RoutedGen::Pool(g) => g.generate(request, session_id, interrupt, tx),
            RoutedGen::Remote(g) => g.generate(request, session_id, interrupt, tx),
        }
    }
}

/// Routes a generation to the provider assigned to a [`ProviderRole`] at
/// runtime (see module docs above).
pub struct RuntimeRouter<'a> {
    pool: &'a EnginePool,
    registry: ProviderRegistry,
    event_tx: Sender<WorkerEvent>,
    remote: HashMap<ProviderRole, Box<dyn TextGenerator>>,
}

impl<'a> RuntimeRouter<'a> {
    /// Build a router over the primary pool + a provider registry snapshot.
    pub fn new(
        pool: &'a EnginePool,
        registry: ProviderRegistry,
        event_tx: Sender<WorkerEvent>,
    ) -> Self {
        Self {
            pool,
            registry,
            event_tx,
            remote: HashMap::new(),
        }
    }

    /// Return a generator suitable for role `role`. Local-routed roles get a
    /// handle into the shared pool (so KV-cache/worker semantics are preserved);
    /// remote-routed roles get a cached per-role [`RemoteGenerator`].
    pub fn resolve(&mut self, role: ProviderRole) -> Result<RoutedGen<'_>, String> {
        match self.registry.route(role) {
            Some(p) if p.kind != ProviderKind::Local => {
                if !self.remote.contains_key(&role) {
                    let cfg = p.to_remote_model_config().map_err(|e| {
                        format!("Role `{role}` provider `{}` is not usable: {e}", p.id)
                    })?;
                    let gen = RemoteGenerator::new(cfg)
                        .map(|g| Box::new(g) as Box<dyn TextGenerator>)?;
                    self.remote.insert(role, gen);
                }
                Ok(RoutedGen::Remote(
                    self.remote
                        .get_mut(&role)
                        .expect("provider inserted above")
                        .as_mut(),
                ))
            }
            // Local (or no matching provider → default fallback): reuse pool.
            _ => Ok(RoutedGen::Pool(self.pool.handle(0))),
        }
    }
}

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
            cached_tokens: Vec::new(),
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
        LlamaBackend::init().map_err(|e| format!("Failed to initialize the llama backend: {e}"))?,
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

/// Render the request's structured messages through the model's own baked-in
/// chat template. Returns `None` when the request carries no messages, the
/// model has no template metadata (e.g. base models), or template application
/// fails — in every fallback case the flat `prompt` string is used as-is.
fn render_chat_template(engine: &StandaloneEngine, request: &InferenceRequest) -> Option<String> {
    let msgs = request.messages.as_ref()?;
    if msgs.is_empty() {
        return None;
    }
    let template = engine.model.chat_template(None).ok()?;
    // Chat templates only understand system / user / assistant roles; our
    // extra roles (context, rules, skill, plan, tool …) carry their section
    // headers inside the content, so they map safely onto `system`.
    let chat: Vec<LlamaChatMessage> = msgs
        .iter()
        .filter(|t| !t.content.trim().is_empty())
        .filter_map(|t| {
            let role = match t.role.as_str() {
                "user" => "user",
                "assistant" => "assistant",
                _ => "system",
            };
            LlamaChatMessage::new(role.to_string(), t.content.clone()).ok()
        })
        .collect();
    if chat.is_empty() {
        return None;
    }
    // add_ass = true leaves the assistant tag open so completion continues it.
    engine
        .model
        .apply_chat_template(&template, &chat, true)
        .ok()
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

    // Prefer the model's own chat template when structured messages were
    // supplied; otherwise complete the flat prompt verbatim.
    let effective_prompt =
        render_chat_template(engine, request).unwrap_or_else(|| request.prompt.clone());

    let prompt_tokens = engine
        .model
        .str_to_token(&effective_prompt, AddBos::Always)
        .map_err(|e| format!("Failed to tokenize prompt: {e}"))?;
    let prompt_len = prompt_tokens.len() as i32;
    if prompt_len >= n_ctx {
        return Err(format!(
            "Prompt is {prompt_len} tokens but the context is only {n_ctx} tokens"
        ));
    }

    // KV-cache prefix reuse is DISABLED across agent-loop calls. The naive
    // reuse (resume at `cached_prefix_tokens` / the LCP of the previous prompt)
    // leaves the llama.cpp KV cache with positions that are not strictly
    // consecutive with what the model expects, so the very next decode returns
    // `-1` (surfaced by the crate as "Decode Error -1: n_tokens == 0" — a
    // generic failure, not actually an empty batch) and multi-step agentic
    // replies break on step 2. Re-evaluating the full prompt every step is
    // correct (consecutive positions 0..n) at the cost of some speed.
    let _ = request.cached_prefix_tokens;
    engine.context.clear_kv_cache();
    engine.cached_tokens.clear();
    let prefix_len = 0usize;
    engine.context.reset_timings();

    let mut sampler = build_sampler(request);

    // The prompt can easily exceed a single LlamaBatch (capacity = n_batch,
    // 512). Chunk it so every `decode` call stays within the batch capacity;
    // logits are only requested on the final prompt token so generation starts
    // from the correct KV position.
    const PROMPT_BATCH: usize = 512;
    let mut batch = LlamaBatch::new(PROMPT_BATCH, 1);
    let mut pos = prefix_len;
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

    // If the prompt was entirely served from the KV cache, the loop above never
    // ran and `batch` is still empty. Feeding that to generation samples from
    // an invalid index (`batch.n_tokens() - 1 == -1`) on stale logits, which
    // derails the queue and surfaces as `n_tokens == 0` decode failures on
    // later agent steps. Re-evaluate just the final prompt token: it is the
    // same token at the same KV position, so the write is idempotent, and it
    // yields fresh logits at a valid sampler index for this request.
    if batch.n_tokens() == 0 {
        let last = prompt_tokens.len() - 1;
        batch
            .add(prompt_tokens[last], last as i32, &[0], true)
            .map_err(|e| format!("Failed to queue cached-prompt token: {e}"))?;
        engine
            .context
            .decode(&mut batch)
            .map_err(|e| format!("Prompt evaluation (cached tail) failed: {e}"))?;
    }

    // The whole prompt is now in the KV cache. Rebuild the ledger so the
    // in-cache token sequence stays authoritative for any future (opt-in)
    // prefix reuse; with reuse currently disabled this is just the prompt.
    engine.cached_tokens = prompt_tokens.clone();

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
            engine.cached_tokens.push(token);
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

        tx.send(WorkerEvent::Token {
            session_id,
            delta: piece,
        })
        .map_err(|e| format!("Token stream channel closed: {e}"))?;

        engine.cached_tokens.push(token);
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

    // Report actual cache statistics: `prefix_len` tokens were served from the
    // reused KV cache; the rest of the prompt plus the outputs were written.
    let cache_reads = prefix_len as u64;
    let cache_writes = ((prompt_len as usize - prefix_len) as u64) + total_tokens;
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
            cache_read_tokens: cache_reads,
            cache_write_tokens: cache_writes,
            reasoning_tokens: 0,
        },
        full_text,
    })
}

/// Build the sampler chain. Must end with a token-selecting sampler
/// (`greedy` or `dist`); penalties/temperature/`top_p` are applied beforehand,
/// mirroring llama.cpp's own default chain order.
fn build_sampler(request: &InferenceRequest) -> LlamaSampler {
    let temperature = request.temperature.unwrap_or(0.8).clamp(0.0, 4.0);
    let top_p = request.top_p.unwrap_or(0.95).clamp(0.0, 1.0);
    let repeat_penalty = request
        .repeat_penalty
        .unwrap_or(REPEAT_PENALTY_DEFAULT)
        .clamp(1.0, 2.0);
    let seed = request.seed.unwrap_or(u32::MAX);

    let mut samplers: Vec<LlamaSampler> = Vec::with_capacity(4);
    if repeat_penalty > 1.0 {
        samplers.push(LlamaSampler::penalties(
            REPEAT_LAST_N,
            repeat_penalty,
            0.0,
            0.0,
        ));
    }
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

    /// The frontend sends camelCase JSON; `messages` must deserialize when
    /// present AND default to `None` when absent (older callers).
    #[test]
    fn inference_request_messages_wire_compat() {
        let with: InferenceRequest = serde_json::from_str(
            r#"{"prompt":"p","messages":[{"role":"user","content":"hi"}],"maxTokens":8}"#,
        )
        .expect("parse with messages");
        let msgs = with.messages.expect("messages present");
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].role, "user");
        assert_eq!(msgs[0].content, "hi");

        let without: InferenceRequest =
            serde_json::from_str(r#"{"prompt":"p","maxTokens":8}"#).expect("parse without");
        assert!(without.messages.is_none());
    }

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
                Ok(Box::new(FakeGen {
                    tag: "worker".into(),
                }) as Box<dyn TextGenerator>)
            },
            ev_tx.clone(),
            2,
        )
        .expect("spawn pool");
        assert_eq!(pool.len(), 2);
        assert_eq!(pool.info().name, "worker");

        let request = InferenceRequest {
            prompt: "p".into(),
            messages: None,
            images: None,
            max_tokens: 8,
            temperature: None,
            top_p: None,
            repeat_penalty: None,
            seed: None,
            stop_words: None,
            cached_prefix_tokens: None,
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

        let o1 = h1
            .join()
            .expect("worker 1 thread")
            .expect("generation 1 ok");
        let o2 = h2
            .join()
            .expect("worker 2 thread")
            .expect("generation 2 ok");
        assert_eq!(o1.full_text, "worker");
        assert_eq!(o2.full_text, "worker");
        assert_eq!(o1.done.total_tokens, 8);
        assert_eq!(o2.done.total_tokens, 8);
    }

    #[test]
    fn runtime_router_routes_local_roles_to_the_pool() {
        let (ev_tx, _ev_rx) = bounded::<WorkerEvent>(256);
        let pool = EnginePool::spawn_with(
            || {
                Ok(Box::new(FakeGen {
                    tag: "pool".into(),
                }) as Box<dyn TextGenerator>)
            },
            ev_tx.clone(),
            1,
        )
        .expect("spawn pool");

        // Local-only registry: every role must fall back to the pool handle.
        let mut router = RuntimeRouter::new(&pool, ProviderRegistry::with_defaults("qwen", None), ev_tx.clone());
        for role in [
            ProviderRole::Planner,
            ProviderRole::Editor,
            ProviderRole::Autocomplete,
            ProviderRole::Embed,
        ] {
            let gen = router.resolve(role).expect("local role resolves");
            assert!(
                matches!(gen, RoutedGen::Pool(_)),
                "role {role:?} must reuse the pool when no remote is routed"
            );
        }

        // A Local provider explicitly mapped to a role still routes to the pool.
        let mut reg = ProviderRegistry::with_defaults("qwen", None);
        let mut local = ProviderConfig::local("qwen");
        local.id = "my_local".into();
        local.roles = vec![ProviderRole::Planner, ProviderRole::Editor];
        reg.add_provider(local.clone());
        reg.set_role_provider(ProviderRole::Editor, "my_local");
        let mut router2 = RuntimeRouter::new(&pool, reg, ev_tx);
        assert!(matches!(router2.resolve(ProviderRole::Editor), Ok(RoutedGen::Pool(_))));
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
            messages: None,
            images: None,
            max_tokens: 48,
            temperature: Some(0.0),
            top_p: Some(1.0),
            repeat_penalty: None,
            seed: Some(42),
            stop_words: Some(vec![]),
            cached_prefix_tokens: None,
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

        eprintln!(
            "prompt_tokens={} total_tokens={} stop_reason={}",
            request.prompt.split_whitespace().count(),
            outcome.done.total_tokens,
            outcome.done.stop_reason
        );
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
            messages: None,
            images: None,
            max_tokens: 32,
            temperature: Some(0.0),
            top_p: Some(1.0),
            repeat_penalty: None,
            seed: Some(42),
            stop_words: Some(vec![]),
            cached_prefix_tokens: None,
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
            messages: None,
            images: None,
            max_tokens: 24,
            temperature: Some(0.0),
            top_p: Some(1.0),
            repeat_penalty: None,
            seed: Some(42),
            stop_words: Some(vec![]),
            cached_prefix_tokens: None,
        };
        let big = run_generation(&mut engine, &big_request, 1, &interrupt, &tx)
            .expect("long-prompt generation must not fail");
        eprintln!(
            "long prompt OK ({} tokens, {})",
            big.done.total_tokens, big.done.stop_reason
        );
        assert!(!big.full_text.trim().is_empty());
    }
}
