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
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use crate::logging;
use crate::remote::RemoteGenerator;
use crossbeam_channel::Sender;
use encoding_rs::UTF_8;
use llama_cpp_2::context::params::{KvCacheType, LlamaContextParams};
use llama_cpp_2::context::LlamaContext;
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::model::{AddBos, LlamaChatMessage, LlamaModel};
use llama_cpp_2::sampling::LlamaSampler;
use llama_cpp_sys_2::llama_flash_attn_type as LlamaFlashAttnType;
use llama_cpp_sys_2::{
    LLAMA_FLASH_ATTN_TYPE_AUTO, LLAMA_FLASH_ATTN_TYPE_DISABLED, LLAMA_FLASH_ATTN_TYPE_ENABLED,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
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
            llama_cpp_sys_2::GGML_LOG_LEVEL_ERROR => {
                push_load_diag(line);
                logging::error(None, "model", line)
            }
            llama_cpp_sys_2::GGML_LOG_LEVEL_WARN => {
                push_load_diag(line);
                logging::warn(None, "model", line)
            }
            _ => logging::info(None, "model", line),
        }
    }
}

/// Recent native WARN/ERROR lines.
///
/// `llama_model_load` only ever hands the Rust wrapper a null pointer, so the
/// actual reason ("tensor name 4 is too long: 71 >= 64", "unexpected EOF",
/// ...) exists solely on this log channel. Keeping the tail lets a failed load
/// report the real cause instead of "null result from llama cpp".
static LOAD_DIAG: Mutex<Vec<String>> = Mutex::new(Vec::new());
const LOAD_DIAG_CAP: usize = 16;

/// Serializes `llama_model_load` calls.
///
/// llama.cpp's log hook is process-global, so the diagnostic lines a load
/// produces cannot be attributed to it unless loads do not interleave. This
/// also guards the backend against two loads mutating shared global state at
/// once, which llama.cpp does not support.
static LOAD_LOCK: Mutex<()> = Mutex::new(());

/// Lock that survives a poisoning panic, so one failed load cannot wedge every
/// later attempt.
fn lock_load() -> std::sync::MutexGuard<'static, ()> {
    LOAD_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

fn push_load_diag(line: &str) {
    if let Ok(mut diag) = LOAD_DIAG.lock() {
        if diag.len() >= LOAD_DIAG_CAP {
            diag.remove(0);
        }
        diag.push(line.to_string());
    }
}

/// Drain the diagnostics collected so far.
fn take_load_diag() -> Vec<String> {
    match LOAD_DIAG.lock() {
        Ok(mut diag) => std::mem::take(&mut *diag),
        Err(_) => Vec::new(),
    }
}

/// Strip llama.cpp's `module: ` tag from the front of a line.
///
/// A prefix only counts as a module tag when it looks like one (all lowercase
/// letters/underscores), so a sentence such as "Tensor data is missing" is left
/// alone.
fn strip_module_tag(line: &str) -> &str {
    match line.find(": ") {
        Some(i)
            if line[..i]
                .chars()
                .all(|c| c.is_ascii_lowercase() || c == '_') =>
        {
            line[i + 2..].trim()
        }
        _ => line.trim(),
    }
}

/// The concrete failure causes to report, in the order llama.cpp emitted them.
///
/// llama.cpp logs cause → consequence → summary (e.g. "tensor name 4 is too
/// long: 71 >= 64", "failed to read tensor info", "error loading model: …"), so
/// the *earliest* lines carry the actionable detail and the trailing summary
/// just repeats "failed to load model". Take the first couple, de-duplicated.
fn load_diag_causes(diag: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in diag {
        let cleaned = strip_module_tag(line);
        if cleaned.is_empty() || out.iter().any(|c| c == cleaned) {
            continue;
        }
        out.push(cleaned.to_string());
        if out.len() == 2 {
            break;
        }
    }
    out
}

/// Extra guidance for failure modes we can recognise, so the message says what
/// to do next rather than only what went wrong.
fn load_hint(diag: &str) -> Option<&'static str> {
    let d = diag.to_ascii_lowercase();
    if d.contains("split file idx") || d.contains("must be loaded with the first split") {
        return Some(
            "this file is one shard of a split model, and only the first shard can be opened on its \
             own. Download every shard (…-00001-of-0000N.gguf, …-00002-of-0000N.gguf, …) into the \
             same folder, then load the -00001-of-0000N.gguf file.",
        );
    }
    if d.contains("is too long") && d.contains("tensor name") {
        return Some(
            "this build of llama.cpp cannot read this GGUF: one of its tensor names exceeds the 64-byte \
             format limit. It is likely a vision/image model or a newer format than the bundled \
             llama.cpp supports — pick a text-only GGUF quant from the hub instead.",
        );
    }
    if d.contains("unexpected eof")
        || d.contains("failed to read tensor data")
        || d.contains("file is too short")
        || d.contains("incomplete")
    {
        return Some(
            "the file on disk is truncated or incomplete — re-download it. If it is one shard of a \
             split model (…-00001-of-0000N.gguf), every shard must be present in the same folder \
             and only the first shard is loadable on its own.",
        );
    }
    if d.contains("unknown architecture") || d.contains("unsupported arch") {
        return Some(
            "this model's architecture is not supported by the bundled llama.cpp — try a GGUF built \
             for a widely supported architecture (llama, qwen2, qwen3, phi, gemma, …).",
        );
    }
    None
}

/// Turn a load failure into a message a user can act on.
fn format_load_error(e: llama_cpp_2::LlamaModelLoadError, diag: &[String]) -> String {
    let causes = load_diag_causes(diag);
    if causes.is_empty() {
        // No native diagnostics: fall back to whatever the wrapper reported.
        return format!("Failed to load model: {e}");
    }
    let reason = causes.join(" / ");
    match load_hint(&reason) {
        Some(hint) => format!("Failed to load model: {reason} — {hint}"),
        None => format!("Failed to load model: {reason}"),
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

/// Compile-time and runtime capabilities of the bundled llama.cpp build.
///
/// The pinned bindings ship neither a version constant nor a "system info"
/// accessor in the crate API, but the raw symbols are available: the
/// `llama_supports_*` probes and `llama_print_system_info` are read directly.
/// `system_info` is llama.cpp's own one-line build/backend summary
/// (e.g. `AVX2 = 1 | AVX = 1 | CUDA = 0 | …`), so the Settings page can show
/// what the binary will actually run rather than guessing from feature flags.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LlamaBuildInfo {
    /// Pinned `llama-cpp-2` / `llama-cpp-sys-2` crate version the binary links.
    pub bindings_version: &'static str,
    /// llama.cpp's own build summary line (may be empty if unavailable).
    pub system_info: String,
    /// The runtime reports at least one GPU-capable device.
    pub supports_gpu_offload: bool,
    pub supports_mmap: bool,
    pub supports_mlock: bool,
    /// Number of backend devices llama.cpp registered (CPU counts as one).
    pub device_count: usize,
    /// Maximum GPU devices llama.cpp was compiled to address.
    pub max_devices: usize,
    /// Single-token decode scratch buffer size in MiB (`llama_max_nodes`-era
    /// probe replaced by the documented 128 MiB default when unavailable).
    pub compute_buffer_mib: f64,
    /// Whether the *binary* was compiled with a GPU backend enabled.
    pub gpu_feature_enabled: bool,
}

/// The crate version the binary was linked against. Kept in one place so the
/// build info cannot drift from `Cargo.toml` silently.
pub const LLAMA_BINDINGS_VERSION: &str = "0.1.154";

/// Probe the bundled llama.cpp build. Safe to call at any time; the backend is
/// initialized lazily by the first model load, but these symbols do not require
/// it (they read compile-time constants or static registries).
pub fn llama_build_info() -> LlamaBuildInfo {
    let system_info = unsafe {
        let ptr = llama_cpp_sys_2::llama_print_system_info();
        if ptr.is_null() {
            String::new()
        } else {
            std::ffi::CStr::from_ptr(ptr).to_string_lossy().into_owned()
        }
    };
    unsafe {
        LlamaBuildInfo {
            bindings_version: LLAMA_BINDINGS_VERSION,
            system_info,
            supports_gpu_offload: llama_cpp_sys_2::llama_supports_gpu_offload(),
            supports_mmap: llama_cpp_sys_2::llama_supports_mmap(),
            supports_mlock: llama_cpp_sys_2::llama_supports_mlock(),
            device_count: llama_cpp_sys_2::ggml_backend_dev_count(),
            max_devices: llama_cpp_sys_2::llama_max_devices(),
            // llama.cpp allocates a fixed 128 MiB graph/compute scratch buffer
            // per context by default; reported for the KV/memory estimate panel.
            compute_buffer_mib: 128.0,
            gpu_feature_enabled: cfg!(any(feature = "gpu-cuda", feature = "gpu-metal")),
        }
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
    /// Seed used for local generations when the request carries no seed
    /// (`None` = random per request).
    default_seed: Option<u32>,
    /// Custom chat template from the config, replacing the model's baked-in
    /// one when set.
    chat_template_override: Option<String>,
    /// Optional instruction prepended to the rendered prompt (set from the
    /// "reasoning budget" advanced setting).
    reasoning_budget: Option<String>,
    /// llama.cpp `n_ubatch` for this context — every `decode` must stay within
    /// this size (and within `n_batch`, which is always >= `n_ubatch`).
    decode_batch: u32,
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

/// User-supplied model loading parameters (camelCase over the wire and under
/// the persisted `localModelConfigs` map in `settings.json`).
///
/// Every field is optional: `None` means "runtime default / auto" so old
/// callers and legacy settings keep working unchanged. Newer callers send the
/// full normalized config produced by the Local Model settings page.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelInitParams {
    /// Layers offloaded to the GPU (0 = CPU only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub n_gpu_layers: Option<u32>,
    /// Requested context (KV cache) size in tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_size: Option<u32>,
    /// Compute threads; defaults to all available cores.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub n_threads: Option<u32>,
    /// How many parallel engine workers (each with its own context) to spawn.
    /// Defaults to 2; threads are split across workers so the machine is not
    /// oversubscribed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub n_workers: Option<u32>,
    /// Evaluation batch size (llama.cpp `n_batch`). Defaults to 512.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub n_batch: Option<u32>,
    /// Physical batch size (llama.cpp `n_ubatch`). Defaults to `n_batch`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub n_ubatch: Option<u32>,
    /// Unified KV cache (uKVC). Defaults to the runtime's default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unified_kv: Option<bool>,
    /// Flash attention. `None` lets the runtime decide (AUTO).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flash_attn: Option<bool>,
    /// Offload the KV cache to GPU memory (llama.cpp `--no-kv-offload`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offload_kqv: Option<bool>,
    /// Try memory-mapping the weights (`--mmap`). Defaults to on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub use_mmap: Option<bool>,
    /// Keep the model locked in memory (`--mlock`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub use_mlock: Option<bool>,
    /// RoPE frequency base override. `None` = automatic.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rope_freq_base: Option<f32>,
    /// RoPE frequency scale override. `None` = automatic.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rope_freq_scale: Option<f32>,
    /// K-cache quantization type (e.g. `Q8_0`, `Q4_0`, `F16`). `None` = auto.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_type_k: Option<String>,
    /// V-cache quantization type (e.g. `Q8_0`, `Q4_0`, `F16`). `None` = auto.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_type_v: Option<String>,
    /// Seed used for local generations when the request itself has no seed.
    /// `None` = random per request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<u32>,
    /// Generation default: max output tokens when a request leaves it unset.
    /// Signed so the frontend's `-1` (infinite) round-trips; `None` (unset)
    /// also means infinite/context-limited.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<i32>,
    /// Speculative decoding mode. Only `"off"`/`None` is currently available;
    /// the field is kept so future modes (draft model, MTP) slot in without a
    /// schema change.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speculative: Option<String>,
    /// Number of context checkpoints (llama.cpp `--n-kv-checkpoints`).
    /// Not exposed by the bundled llama.cpp bindings yet; reserved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_checkpoints: Option<u32>,
    /// Custom chat template used in place of the model's embedded one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat_template: Option<String>,
    /// Optional instruction message prepended to the rendered prompt for
    /// reasoning-budget behaviour.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_budget: Option<String>,
    /// Raw llama.cpp argument overrides (space separated). Recognized flags
    /// map straight onto the fields above and take precedence over them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub llama_args: Option<String>,
}

impl ModelInitParams {
    /// Overlay the non-`None` fields of `explicit` on top of `self`, returning
    /// the merged result. Used to resolve: defaults ← legacy settings ←
    /// persisted per-model config ← explicit load call.
    pub fn merge(&self, explicit: &ModelInitParams) -> ModelInitParams {
        let mut out = self.clone();
        if let Some(v) = explicit.n_gpu_layers {
            out.n_gpu_layers = Some(v);
        }
        if let Some(v) = explicit.context_size {
            out.context_size = Some(v);
        }
        if let Some(v) = explicit.n_threads {
            out.n_threads = Some(v);
        }
        if let Some(v) = explicit.n_workers {
            out.n_workers = Some(v);
        }
        if let Some(v) = explicit.n_batch {
            out.n_batch = Some(v);
        }
        if let Some(v) = explicit.n_ubatch {
            out.n_ubatch = Some(v);
        }
        if let Some(v) = explicit.unified_kv {
            out.unified_kv = Some(v);
        }
        if let Some(v) = explicit.flash_attn {
            out.flash_attn = Some(v);
        }
        if let Some(v) = explicit.offload_kqv {
            out.offload_kqv = Some(v);
        }
        if let Some(v) = explicit.use_mmap {
            out.use_mmap = Some(v);
        }
        if let Some(v) = explicit.use_mlock {
            out.use_mlock = Some(v);
        }
        if let Some(v) = explicit.rope_freq_base {
            out.rope_freq_base = Some(v);
        }
        if let Some(v) = explicit.rope_freq_scale {
            out.rope_freq_scale = Some(v);
        }
        if let Some(v) = &explicit.cache_type_k {
            out.cache_type_k = Some(v.clone());
        }
        if let Some(v) = &explicit.cache_type_v {
            out.cache_type_v = Some(v.clone());
        }
        if let Some(v) = explicit.seed {
            out.seed = Some(v);
        }
        if let Some(v) = explicit.max_tokens {
            out.max_tokens = Some(v);
        }
        if let Some(v) = &explicit.speculative {
            out.speculative = Some(v.clone());
        }
        if let Some(v) = explicit.context_checkpoints {
            out.context_checkpoints = Some(v);
        }
        if let Some(v) = &explicit.chat_template {
            out.chat_template = Some(v.clone());
        }
        if let Some(v) = &explicit.reasoning_budget {
            out.reasoning_budget = Some(v.clone());
        }
        if let Some(v) = &explicit.llama_args {
            out.llama_args = Some(v.clone());
        }
        out
    }

    /// Basic range validation before the model is handed to llama.cpp. Keeps
    /// obviously-broken configurations from surfacing as cryptic runtime
    /// errors. Context and GPU-layer clamping against the actual model happens
    /// after load (see [`load_model_with_progress`]).
    pub fn validate(&self) -> Result<(), String> {
        if let Some(v) = self.n_gpu_layers {
            if v > 16_384 {
                return Err(format!("GPU layers out of range: {v} (max 16384)"));
            }
        }
        if let Some(v) = self.n_threads {
            if !(1..=1024).contains(&v) {
                return Err(format!("Threads must be between 1 and 1024 (got {v})"));
            }
        }
        if let Some(v) = self.n_workers {
            if !(1..=32).contains(&v) {
                return Err(format!("Max concurrent must be between 1 and 32 (got {v})"));
            }
        }
        for (label, v) in [
            ("Evaluation batch size", self.n_batch),
            ("Physical batch size", self.n_ubatch),
        ] {
            if let Some(v) = v {
                if v < 1 || v > 1_048_576 {
                    return Err(format!("{label} out of range: {v} (1..1048576)"));
                }
            }
        }
        if let (Some(batch), Some(ubatch)) = (self.n_batch, self.n_ubatch) {
            if ubatch > batch {
                return Err(format!(
                    "Physical batch size ({ubatch}) cannot exceed the evaluation batch size ({batch})"
                ));
            }
        }
        if let Some(v) = self.rope_freq_base {
            if !(0.1..=1_000_000.0).contains(&v) {
                return Err(format!(
                    "RoPE frequency base out of range: {v} (0.1..1000000)"
                ));
            }
        }
        if let Some(v) = self.rope_freq_scale {
            if !(0.01..=1_000.0).contains(&v) {
                return Err(format!(
                    "RoPE frequency scale out of range: {v} (0.01..1000)"
                ));
            }
        }
        for (label, v) in [
            ("K-cache quantization", self.cache_type_k.as_deref()),
            ("V-cache quantization", self.cache_type_v.as_deref()),
        ] {
            if let Some(v) = v {
                if parse_cache_type(v).is_none() {
                    return Err(format!(
                        "Unsupported {label} type `{v}` (expected one of: {})",
                        CACHE_TYPES.join(", ")
                    ));
                }
            }
        }
        if let Some(v) = self.seed {
            if v >= u32::MAX {
                return Err(format!("Seed must be below {0} (got {v})", u32::MAX));
            }
        }
        if let Some(spec) = self.speculative.as_deref() {
            if !matches!(spec, "off" | "") {
                return Err(format!(
                    "Speculative decoding mode `{spec}` is not supported (only \"off\" is available)"
                ));
            }
        }
        Ok(())
    }

    /// Apply a raw llama.cpp argument string: each recognized flag is parsed
    /// and folded onto the matching field (taking precedence over the UI
    /// value). Unknown flags are rejected so a typo can never be silently
    /// ignored. See [`apply_llama_args`].
    pub fn apply_llama_args(&mut self, args: &str) -> Result<(), String> {
        apply_llama_args(self, args)
    }
}

/// Cache quantization types accepted by the KV-cache override controls.
pub const CACHE_TYPES: &[&str] = &[
    "F16", "F32", "Q8_0", "Q4_0", "Q4_1", "Q5_0", "Q5_1", "IQ4_NL",
];

/// Map a cache-quantization type name onto the crate's [`KvCacheType`].
pub fn parse_cache_type(name: &str) -> Option<KvCacheType> {
    match &name.trim().to_uppercase().replace('.', "_")[..] {
        "F16" => Some(KvCacheType::F16),
        "F32" => Some(KvCacheType::F32),
        "Q4_0" => Some(KvCacheType::Q4_0),
        "Q4_1" => Some(KvCacheType::Q4_1),
        "Q5_0" => Some(KvCacheType::Q5_0),
        "Q5_1" => Some(KvCacheType::Q5_1),
        "Q8_0" => Some(KvCacheType::Q8_0),
        "Q8_1" => Some(KvCacheType::Q8_1),
        "Q2_K" => Some(KvCacheType::Q2_K),
        "Q3_K" => Some(KvCacheType::Q3_K),
        "Q4_K" => Some(KvCacheType::Q4_K),
        "Q5_K" => Some(KvCacheType::Q5_K),
        "Q6_K" => Some(KvCacheType::Q6_K),
        "Q8_K" => Some(KvCacheType::Q8_K),
        "IQ1_S" => Some(KvCacheType::IQ1_S),
        "IQ2_S" => Some(KvCacheType::IQ2_S),
        "IQ2_XS" => Some(KvCacheType::IQ2_XS),
        "IQ2_XXS" => Some(KvCacheType::IQ2_XXS),
        "IQ3_S" => Some(KvCacheType::IQ3_S),
        "IQ3_XXS" => Some(KvCacheType::IQ3_XXS),
        "IQ4_NL" => Some(KvCacheType::IQ4_NL),
        "IQ4_XS" => Some(KvCacheType::IQ4_XS),
        "IQ1_M" => Some(KvCacheType::IQ1_M),
        "BF16" => Some(KvCacheType::BF16),
        _ => None,
    }
}

/// Parse a raw `llama.cpp` argument override string onto [`ModelInitParams`].
/// Values are applied last, so a flag here overrides the corresponding UI
/// field. Flag aliases mirror llama.cpp's own CLI. Unknown flags produce a
/// clear error instead of being silently dropped.
fn apply_llama_args(params: &mut ModelInitParams, args: &str) -> Result<(), String> {
    let mut tokens = args.split_whitespace();
    let need_next =
        |flag: &str, tokens: &mut std::str::SplitWhitespace| -> Result<String, String> {
            tokens
                .next()
                .ok_or_else(|| format!("`{flag}` requires a value"))
                .map(|v| v.to_string())
        };
    while let Some(tok) = tokens.next() {
        let (flag, inline_val) = match tok.split_once('=') {
            Some((f, v)) => (f, Some(v.to_string())),
            None => (tok, None),
        };
        let take = |tokens: &mut std::str::SplitWhitespace| -> Result<String, String> {
            match inline_val.clone() {
                Some(v) => Ok(v),
                None => need_next(flag, tokens),
            }
        };
        match flag {
            "-c" | "--ctx-size" | "--context-size" => {
                let v = take(&mut tokens)?
                    .parse::<u32>()
                    .map_err(|_| "`--ctx-size` must be a number".to_string())?;
                params.context_size = Some(v);
            }
            "-t" | "--threads" => {
                let v = take(&mut tokens)?
                    .parse::<u32>()
                    .map_err(|_| "`--threads` must be a number".to_string())?;
                params.n_threads = Some(v);
            }
            "-ngl" | "--gpu-layers" | "--n-gpu-layers" => {
                let v = take(&mut tokens)?
                    .parse::<u32>()
                    .map_err(|_| "`--gpu-layers` must be a number".to_string())?;
                params.n_gpu_layers = Some(v);
            }
            "-b" | "--batch-size" => {
                let v = take(&mut tokens)?
                    .parse::<u32>()
                    .map_err(|_| "`--batch-size` must be a number".to_string())?;
                params.n_batch = Some(v);
            }
            "-ub" | "--ubatch-size" | "--physical-batch" => {
                let v = take(&mut tokens)?
                    .parse::<u32>()
                    .map_err(|_| "`--ubatch-size` must be a number".to_string())?;
                params.n_ubatch = Some(v);
            }
            "-np" | "--parallel" | "--concurrency" => {
                let v = take(&mut tokens)?
                    .parse::<u32>()
                    .map_err(|_| "`--parallel` must be a number".to_string())?;
                params.n_workers = Some(v);
            }
            "-s" | "--seed" => {
                let v = take(&mut tokens)?.parse::<u32>().map_err(|_| {
                    "`--seed` must be a number between 0 and 4294967294".to_string()
                })?;
                params.seed = Some(v);
            }
            "--rope-freq-base" | "--rope-frequency-base" => {
                let v = take(&mut tokens)?
                    .parse::<f32>()
                    .map_err(|_| "`--rope-freq-base` must be a number".to_string())?;
                params.rope_freq_base = Some(v);
            }
            "--rope-freq-scale" | "--rope-frequency-scale" => {
                let v = take(&mut tokens)?
                    .parse::<f32>()
                    .map_err(|_| "`--rope-freq-scale` must be a number".to_string())?;
                params.rope_freq_scale = Some(v);
            }
            "--cache-type-k" => {
                let v = take(&mut tokens)?;
                params.cache_type_k = Some(v);
            }
            "--cache-type-v" => {
                let v = take(&mut tokens)?;
                params.cache_type_v = Some(v);
            }
            "--chat-template" => {
                let v = take(&mut tokens)?;
                params.chat_template = Some(v);
            }
            "--flash-attn" | "--flash-attention" => params.flash_attn = Some(true),
            "--no-flash-attn" | "--no-flash-attention" => params.flash_attn = Some(false),
            "--kv-offload" | "--offload-kqv" => params.offload_kqv = Some(true),
            "--no-kv-offload" | "--no-offload-kqv" => params.offload_kqv = Some(false),
            "--mmap" => params.use_mmap = Some(true),
            "--no-mmap" => params.use_mmap = Some(false),
            "--mlock" => params.use_mlock = Some(true),
            "--no-mlock" => params.use_mlock = Some(false),
            "--unified-kv" | "--ukvc" | "--use-unified-kv" => params.unified_kv = Some(true),
            "--no-unified-kv" => params.unified_kv = Some(false),
            other => {
                return Err(format!(
                    "Unsupported llama.cpp argument override `{other}`. Supported flags: --ctx-size/-c, --threads/-t, --gpu-layers/-ngl, --batch-size/-b, --ubatch-size/-ub, --parallel/-np, --seed/-s, --rope-freq-base, --rope-freq-scale, --cache-type-k, --cache-type-v, --chat-template, --flash-attn/--no-flash-attn, --kv-offload/--no-kv-offload, --mmap/--no-mmap, --mlock/--no-mlock, --unified-kv/--no-unified-kv"
                ));
            }
        }
    }
    Ok(())
}

/// One structured conversation turn used to render the prompt through the
/// model's own chat template (see [`InferenceRequest::messages`]).
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
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
}

/// A single streaming inference request (camelCase over the wire).
#[derive(Debug, Clone, Default, Deserialize)]
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
    pub stop_words: Option<Vec<String>>,
    /// Number of prompt tokens already present in the KV cache from a previous
    /// generation call on the same worker. When set, the engine skips clearing
    /// the cache and only processes tokens from this offset onward, saving
    /// prompt re-evaluation cost on multi-step agent loops.
    #[serde(default)]
    pub cached_prefix_tokens: Option<usize>,
    /// Native tool definitions in OpenAI `tools` shape
    /// (`{"type":"function","function":{"name","description","parameters"}}`).
    ///
    /// Only consumed by remote generators: the local llama.cpp path has no
    /// tool channel and keeps parsing `<execute_tool>` markdown out of the
    /// generated text. A remote provider that returns structured `tool_calls`
    /// has them re-rendered as `<execute_tool>` blocks in `GenerationOutcome`,
    /// so the orchestrator's parser handles both paths identically.
    #[serde(default)]
    pub tools: Option<Vec<Value>>,
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
    /// Generation was accepted and prompt evaluation is about to begin.
    ///
    /// Emitted before the first decode so a listener can timestamp the *real*
    /// start of the turn. Without it the time-to-first-token below included
    /// prompt evaluation but was measured from the first delta, reporting a
    /// misleading `0 ms` for what is usually the slowest phase.
    Started {
        session_id: u64,
        /// Prompt length in tokens, or 0 when the backend does not tokenize
        /// locally (remote providers count tokens server-side).
        prompt_tokens: usize,
    },
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
    /// Render `request.messages` through this backend's chat template, when it
    /// has one. Defaults to `None` (remote generators ship their own message
    /// formatting); the local engine renders through the model's embedded
    /// template or the configured override.
    fn render_chat(&mut self, _request: &InferenceRequest) -> Option<String> {
        None
    }
    /// Does this backend have a native tool channel (OpenAI `tools`)?
    ///
    /// Defaults to `false`, which is right for the local llama.cpp path: it has
    /// no tool-call API and reads `<execute_tool>` markdown from the generated
    /// text instead. The orchestrator skips building a `tools` catalog for
    /// backends that answer `false`, so no local prompt changes shape.
    fn supports_native_tools(&self) -> bool {
        false
    }
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

    fn render_chat(&mut self, request: &InferenceRequest) -> Option<String> {
        render_chat_template(&self.engine, request)
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
    /// Streaming generation that routes token events to a caller-supplied
    /// channel instead of the pool's shared UI bus. Used by the local
    /// OpenAI-compatible server so an HTTP SSE stream owns its own events.
    Stream {
        request: InferenceRequest,
        session_id: u64,
        interrupt: CancellationToken,
        events: crossbeam_channel::Sender<WorkerEvent>,
        reply: crossbeam_channel::Sender<Result<GenerationOutcome, String>>,
    },
    /// Render the model's chat template for a message list (local models only;
    /// remote generators answer `None`). Needed by the API server so
    /// `POST /v1/chat/completions` feeds the model the exact instruction format
    /// it was tuned with — the same template the agent loop uses.
    RenderChat {
        request: InferenceRequest,
        reply: crossbeam_channel::Sender<Option<String>>,
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
    /// Whether the pool's backend has a native tool channel. Captured from the
    /// inner generator at spawn time; the handle cannot reach the worker's
    /// generator, so the flag must travel with the handle.
    native_tools: bool,
}

impl PoolGenerator {
    fn new(tx: crossbeam_channel::Sender<EngineMsg>, info: ModelInfo, native_tools: bool) -> Self {
        Self {
            tx,
            info,
            native_tools,
        }
    }
}

impl TextGenerator for PoolGenerator {
    fn info(&self) -> ModelInfo {
        self.info.clone()
    }

    fn supports_native_tools(&self) -> bool {
        self.native_tools
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

impl PoolGenerator {
    /// Streaming generation routed to caller-owned event + outcome channels.
    /// The events channel carries [`WorkerEvent::Token`] deltas (tagged with
    /// `session_id`) and ends with [`WorkerEvent::Done`]; the outcome channel
    /// yields the final [`GenerationOutcome`]. Both channels are closed by the
    /// worker when the generation finishes or the engine is unloaded.
    pub fn stream(
        &self,
        request: InferenceRequest,
        session_id: u64,
        interrupt: CancellationToken,
    ) -> Result<
        (
            crossbeam_channel::Receiver<WorkerEvent>,
            crossbeam_channel::Receiver<Result<GenerationOutcome, String>>,
        ),
        String,
    > {
        let (events_tx, events_rx) = crossbeam_channel::unbounded::<WorkerEvent>();
        let (reply_tx, reply_rx) =
            crossbeam_channel::bounded::<Result<GenerationOutcome, String>>(1);
        self.tx
            .send(EngineMsg::Stream {
                request,
                session_id,
                interrupt,
                events: events_tx,
                reply: reply_tx,
            })
            .map_err(|_| "Engine worker is gone".to_string())?;
        Ok((events_rx, reply_rx))
    }

    /// Render `request.messages` through the model's chat template on the
    /// worker. `None` when the backend is remote or carries no template — the
    /// API server then falls back to its flat-prompt flattening.
    pub fn render_chat(&self, request: &InferenceRequest) -> Option<String> {
        let (reply_tx, reply_rx) = crossbeam_channel::bounded::<Option<String>>(1);
        if self
            .tx
            .send(EngineMsg::RenderChat {
                request: request.clone(),
                reply: reply_tx,
            })
            .is_err()
        {
            return None;
        }
        reply_rx.recv().ok().flatten()
    }
}

struct EngineWorker {
    tx: crossbeam_channel::Sender<EngineMsg>,
    info: ModelInfo,
    native_tools: bool,
    _handle: std::thread::JoinHandle<()>,
}

// ---------------------------------------------------------------------------
// Runtime role routing (model hand-off + per-role worker pools).
//
// The [`EnginePool`] is built from one backend (usually local GGUF). The
// [`RuntimeRouter`] lets the orchestrator hand a generation off to a different
// provider mid-task by role: e.g. a cheap/fast remote model plans while the
// pool's flagship model edits. Local routes (the common case) borrow a worker
// handle from the primary pool; remote-routed roles lazily spawn their own
// small worker pool ([`ROLE_POOL_WORKERS`] workers each, one HTTP client per
// worker) so a single role can serve several generations concurrently. All
// routed steps hand out owned [`PoolGenerator`] handles, so the orchestrator
// cannot tell a local from a remote generation. The router lives on the
// orchestrator's own thread; the only shared state (the event channel) is
// `Clone`-cheap.
// ---------------------------------------------------------------------------

/// How many worker threads a routed remote role's pool starts with. Each
/// worker owns its own HTTP client, so concurrent generations on one role
/// (e.g. parallel Planner subtasks) don't block each other.
const ROLE_POOL_WORKERS: usize = 2;

/// Routes a generation to the provider assigned to a [`ProviderRole`] at
/// runtime (see module docs above).
pub struct RuntimeRouter<'a> {
    /// The primary engine pool (usually the local GGUF).
    pool: &'a EnginePool,
    /// Snapshot of the provider registry (+ role → provider map).
    registry: ProviderRegistry,
    /// Worker-event channel cloned into every role-pool worker so remote
    /// token streams surface in the UI exactly like local ones.
    event_tx: Sender<WorkerEvent>,
    /// Lazily spawned worker pool per routed remote role. A role that routes
    /// to a Local provider (or has no provider → default fallback) never
    /// spawns here and instead borrows the primary pool.
    role_pools: HashMap<ProviderRole, EnginePool>,
    /// Round-robin cursor per remote role so consecutive generations spread
    /// across that role's workers.
    next_idx: HashMap<ProviderRole, usize>,
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
            role_pools: HashMap::new(),
            next_idx: HashMap::new(),
        }
    }

    /// Return a generator handle suitable for role `role`. Local-routed roles
    /// lease the primary pool's worker 0 (so KV-cache/worker semantics are
    /// preserved); remote-routed roles lazily spawn a dedicated worker pool
    /// and hand out a handle round-robin across its workers.
    pub fn resolve(&mut self, role: ProviderRole) -> Result<PoolGenerator, String> {
        match self.registry.route(role) {
            Some(p) if p.kind != ProviderKind::Local => {
                if !self.role_pools.contains_key(&role) {
                    let cfg = p.to_remote_model_config().map_err(|e| {
                        format!("Role `{role}` provider `{}` is not usable: {e}", p.id)
                    })?;
                    let event_tx = self.event_tx.clone();
                    let pool = EnginePool::spawn_with(
                        move || {
                            RemoteGenerator::new(cfg.clone())
                                .map(|g| Box::new(g) as Box<dyn TextGenerator>)
                        },
                        event_tx,
                        ROLE_POOL_WORKERS,
                    )?;
                    self.role_pools.insert(role, pool);
                }
                let idx = self.next_idx.entry(role).or_insert(0);
                let handle = self
                    .role_pools
                    .get(&role)
                    .expect("role pool inserted above")
                    .handle(*idx);
                *idx = (*idx + 1) % ROLE_POOL_WORKERS;
                Ok(handle)
            }
            // Local (or no matching provider → default fallback): reuse pool.
            _ => Ok(self.pool.handle(0)),
        }
    }

    /// Number of workers in a routed role's remote pool (tests only).
    #[cfg(test)]
    fn role_pool_len(&self, role: ProviderRole) -> Option<usize> {
        self.role_pools.get(&role).map(EnginePool::len)
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
            let native_tools = inner.supports_native_tools();
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
                        EngineMsg::Stream {
                            request,
                            session_id,
                            interrupt,
                            events,
                            reply,
                        } => {
                            let result = inner.generate(&request, session_id, &interrupt, &events);
                            let _ = reply.send(result);
                        }
                        EngineMsg::RenderChat { request, reply } => {
                            let rendered = inner.render_chat(&request);
                            let _ = reply.send(rendered);
                        }
                        EngineMsg::Stop => break,
                    }
                }
            });
            workers.push(EngineWorker {
                tx,
                info,
                native_tools,
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
        PoolGenerator::new(w.tx.clone(), w.info.clone(), w.native_tools)
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
    /// The (validated, llama-args folded) config this model was loaded with;
    /// `new_engine_with_threads` builds every pool context from it.
    config: ModelInitParams,
}

impl LoadedModel {
    fn flash_policy(flash: Option<bool>, n_gpu_layers: u32) -> LlamaFlashAttnType {
        match flash {
            Some(true) => LLAMA_FLASH_ATTN_TYPE_ENABLED,
            Some(false) => LLAMA_FLASH_ATTN_TYPE_DISABLED,
            None if n_gpu_layers > 0 => LLAMA_FLASH_ATTN_TYPE_AUTO,
            None => LLAMA_FLASH_ATTN_TYPE_DISABLED,
        }
    }

    /// Create a new standalone engine (own context) using a specific thread
    /// count (used to split compute across pool workers).
    pub fn new_engine_with_threads(&self, threads: i32) -> Result<StandaloneEngine, String> {
        let c = &self.config;
        let n_batch = c.n_batch.unwrap_or(512).max(1);
        let n_ubatch = c.n_ubatch.unwrap_or(n_batch).clamp(1, n_batch);
        let n_gpu_layers = c.n_gpu_layers.unwrap_or(0);

        let mut ctx_params = LlamaContextParams::default()
            .with_n_ctx(NonZeroU32::new(self.n_ctx))
            .with_n_threads(threads)
            .with_n_threads_batch(threads)
            .with_n_batch(n_batch)
            .with_n_ubatch(n_ubatch)
            .with_flash_attention_policy(Self::flash_policy(c.flash_attn, n_gpu_layers));
        if let Some(unified) = c.unified_kv {
            ctx_params = ctx_params.with_kv_unified(unified);
        }
        if let Some(v) = c.offload_kqv {
            ctx_params = ctx_params.with_offload_kqv(v);
        }
        if let Some(v) = c.rope_freq_base {
            ctx_params = ctx_params.with_rope_freq_base(v);
        }
        if let Some(v) = c.rope_freq_scale {
            ctx_params = ctx_params.with_rope_freq_scale(v);
        }
        if let Some(kind) = c.cache_type_k.as_deref().and_then(parse_cache_type) {
            ctx_params = ctx_params.with_type_k(kind);
        }
        if let Some(kind) = c.cache_type_v.as_deref().and_then(parse_cache_type) {
            ctx_params = ctx_params.with_type_v(kind);
        }

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
            default_seed: c.seed.filter(|s| *s != u32::MAX),
            chat_template_override: c.chat_template.clone().filter(|t| !t.trim().is_empty()),
            reasoning_budget: c.reasoning_budget.clone().filter(|m| !m.trim().is_empty()),
            decode_batch: n_ubatch,
        })
    }
}

/// llama.cpp's backend can only be initialized once per process — the crate
/// tracks that with a global flag and `LlamaBackend::init()` returns
/// `BackendAlreadyInitialized` on any second call (e.g. when loading another
/// model after unloading one). Cache a single instance for the app lifetime so
/// every model load shares it instead of re-initializing.
fn global_backend() -> Result<Arc<LlamaBackend>, String> {
    static BACKEND: OnceLock<Result<Arc<LlamaBackend>, String>> = OnceLock::new();
    BACKEND
        .get_or_init(|| {
            LlamaBackend::init()
                .map(Arc::new)
                .map_err(|e| format!("Failed to initialize the llama backend: {e}"))
        })
        .clone()
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

    // `.imatrix.gguf` files are quantization calibration data, not models.
    // Skip llama.cpp entirely so the user gets a clear message instead of
    // the cryptic "null result from llama cpp".
    let is_imatrix = path
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase().contains("imatrix"))
        .unwrap_or(false);
    if is_imatrix {
        return Err(format!(
            "{} is a quantization calibration (imatrix) file, not a loadable model — delete it and download the actual model (see the quantized GGUF rows in the hub instead)",
            path.display()
        ));
    }

    let backend = global_backend()?;

    // Validate the requested config before touching llama.cpp so a bad value
    // surfaces as a readable error instead of a crash mid-load. Raw llama.cpp
    // argument overrides are folded in first (they take precedence over the
    // UI fields), then the merged result is validated.
    let mut config = params.clone();
    if let Some(args) = config.llama_args.take() {
        config
            .apply_llama_args(&args)
            .map_err(|e| format!("llama.cpp arguments override failed: {e}"))?;
    }
    config.validate()?;

    let n_gpu_layers = config.n_gpu_layers.unwrap_or(0);
    let requested_ctx = config.context_size.unwrap_or(4096);

    let mut model_params = LlamaModelParams::default()
        .with_n_gpu_layers(n_gpu_layers)
        .with_use_mmap(config.use_mmap.unwrap_or(true))
        .with_progress_callback(move |progress: f32| on_progress(progress));
    if config.use_mlock == Some(true) {
        model_params = model_params.with_use_mlock(true);
    }

    // Loads are serialized so the process-global log hook's lines can be
    // attributed to this attempt. Start from a clean slate so diagnostics from
    // an earlier attempt are not carried over.
    let _load_guard = lock_load();
    take_load_diag();
    let loaded = LlamaModel::load_from_file(&backend, path, &model_params);
    let diag = take_load_diag();
    let model = loaded.map_err(|e| format_load_error(e, &diag))?;

    // Clamp the KV cache to what the model was trained on (never silently
    // allocate more than `n_ctx_train`).
    let n_ctx_train = model.n_ctx_train();
    let n_ctx = requested_ctx.clamp(64, n_ctx_train.max(64));

    Ok(LoadedModel {
        backend,
        model: Arc::new(model),
        n_ctx,
        config,
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
    // A configured reasoning-budget instruction becomes a leading system turn
    // so it is always the first thing in the rendered prompt.
    let mut chat: Vec<LlamaChatMessage> = Vec::with_capacity(msgs.len() + 1);
    if let Some(reasoning) = engine.reasoning_budget.as_deref() {
        if let Ok(m) = LlamaChatMessage::new("system".to_string(), reasoning.to_string()) {
            chat.push(m);
        }
    }
    // Chat templates only understand system / user / assistant roles; our
    // extra roles (context, rules, skill, plan, tool …) carry their section
    // headers inside the content, so they map safely onto `system`.
    for t in msgs.iter().filter(|t| !t.content.trim().is_empty()) {
        let role = match t.role.as_str() {
            "user" => "user",
            "assistant" => "assistant",
            _ => "system",
        };
        if let Ok(m) = LlamaChatMessage::new(role.to_string(), t.content.clone()) {
            chat.push(m);
        }
    }
    if chat.is_empty() {
        return None;
    }
    // A configured custom template replaces the model's baked-in one; it wins
    // even when the model carries no template metadata.
    let template = match engine.chat_template_override.as_deref() {
        Some(raw) => match llama_cpp_2::model::LlamaChatTemplate::new(raw) {
            Ok(t) => t,
            Err(_) => return None,
        },
        None => engine.model.chat_template(None).ok()?,
    };
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

    // Announce the turn before any decode so listeners measure time-to-first
    // token against prompt evaluation, not against the first delta (which would
    // always read 0 ms).
    let _ = tx.send(WorkerEvent::Started {
        session_id,
        prompt_tokens: prompt_tokens.len(),
    });

    // KV-cache prefix reuse across agent-loop calls. The cache is only valid for
    // the exact token sequence that produced it, so reuse is gated on a
    // *verified* longest-common-prefix against the ledger of what is actually
    // in the cache (`cached_tokens` = the previous prompt plus its output).
    //
    // Positions stay strictly consecutive (0..=prefix_len-1 reused, then
    // prefix_len..=n decoded), which is what llama.cpp requires. The earlier
    // "Decode Error -1" failures came from resuming at a caller-supplied offset
    // that did not correspond to a real prefix boundary; deriving the offset
    // from the ledger instead makes that mismatch impossible.
    //
    // At least one prompt token must be decoded to obtain fresh logits, so the
    // reused prefix is capped at `prompt_len - 1`.
    let prefix_len = common_prefix_len(&engine.cached_tokens, &prompt_tokens)
        .min(request.cached_prefix_tokens.unwrap_or(usize::MAX))
        .min((prompt_tokens.len() as i32 - 1).max(0) as usize);

    // Only attempt the partial removal when there is something to reuse;
    // `kv_cache_seq_rm` on an empty boundary is a needless failure mode.
    let truncate_ok = prefix_len == 0
        || engine
            .context
            .kv_cache_seq_rm(0, Some(prefix_len as u32), None)
            .is_ok();
    let plan = plan_kv_reuse(prefix_len, truncate_ok);
    let prefix_len = match plan {
        // Drop everything at or after the reuse boundary. Without this the
        // stale tail from the previous turn would still occupy cells and the
        // next decode would find no room for the new tokens.
        KvReusePlan::Truncate(n) => {
            engine.cached_tokens.truncate(n);
            n
        }
        // Either nothing was reusable, or the partial removal was rejected. Both
        // end in the same place: drop the whole cache and decode from scratch.
        KvReusePlan::Clear => {
            engine.context.clear_kv_cache();
            engine.cached_tokens.clear();
            0
        }
    };
    engine.context.reset_timings();

    let mut sampler = build_sampler(engine.default_seed);

    // Chunk long prompts so each `decode` stays within this context's physical
    // batch (`n_ubatch`). Hard-coding 512 here while the context uses a
    // smaller batch corrupts ggml's heap on Windows.
    let prompt_batch = engine.decode_batch.max(1) as usize;
    let mut batch = LlamaBatch::new(prompt_batch, 1);
    let mut pos = prefix_len;
    while pos < prompt_tokens.len() {
        let end = (pos + prompt_batch).min(prompt_tokens.len());
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
    // in-cache token sequence stays authoritative for the next step's prefix
    // reuse; the generation loop below appends its output tokens to it, so the
    // ledger ends up as exactly the sequence the cache holds.
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

        // A token the model's vocabulary cannot decode (llama.cpp reports
        // `Unknown Token Type`) must not kill the whole generation: a degraded
        // or hybrid-architecture GGUF can occasionally sample an undefined vocab
        // entry. Skip it — still advancing the KV position so the loop cannot
        // spin — and keep generating, exactly as for an empty piece below.
        let piece = match engine
            .model
            .token_to_piece(token, &mut decoder, false, None)
        {
            Ok(p) => p,
            Err(e) => {
                logging::warn(
                    Some(session_id),
                    "model",
                    &format!("skipping undecodable token: {e}"),
                );
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
        };

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
/// Build the exact sampler chain llama.cpp uses by default (`temp 0.8`,
/// `top_k 40`, `top_p 0.95`, `min_p 0.05`, repetition penalty 1.1 over the
/// last 64 tokens) and terminate it with a distribution sampler. No request
/// or model settings can alter it: the local model runs "as is".
fn build_sampler(default_seed: Option<u32>) -> LlamaSampler {
    let seed = default_seed.unwrap_or(u32::MAX).min(u32::MAX - 1);
    LlamaSampler::chain_simple(vec![
        LlamaSampler::penalties(64, 1.1, 0.0, 0.0),
        LlamaSampler::top_k(40),
        LlamaSampler::top_p(0.95, 1),
        LlamaSampler::min_p(0.05, 1),
        LlamaSampler::temp(0.8),
        LlamaSampler::dist(seed),
    ])
}

fn has_stop_suffix(text: &str, stop_words: &[String]) -> bool {
    stop_words
        .iter()
        .any(|w| !w.is_empty() && text.len() >= w.len() && text.ends_with(w.as_str()))
}

/// Length of the longest common prefix of two token sequences — i.e. how many
/// leading tokens of `next` are already resident in the KV cache that produced
/// `cached`.
///
/// This is the only trustworthy reuse boundary. A caller-supplied offset (or
/// the previous prompt's length) can overstate what is really cached, which is
/// what made the earlier reuse attempt decode from the wrong KV position and
/// fail with `n_tokens == 0`.
fn common_prefix_len(
    cached: &[llama_cpp_2::token::LlamaToken],
    next: &[llama_cpp_2::token::LlamaToken],
) -> usize {
    cached
        .iter()
        .zip(next.iter())
        .take_while(|(a, b)| a == b)
        .count()
}

/// What to do with the KV cache before decoding a new prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KvReusePlan {
    /// Nothing reusable: clear the cache and decode from position 0.
    Clear,
    /// Keep `prefix_len` tokens resident and truncate the stale tail.
    Truncate(usize),
}

/// Decide how to prepare the KV cache for a new prompt.
///
/// `truncate_ok` reports whether `kv_cache_seq_rm` succeeded. It returns `false`
/// with "Couldn't remove partial sequence" in the normal state after a
/// decode+generate cycle, because llama.cpp spreads a sequence across cells as
/// it recycles the pool. That is a cache-placement detail rather than a
/// correctness problem — the ledger already proves the first `prefix_len` tokens
/// are genuinely cached — so the caller degrades to a full clear and a
/// re-decode rather than failing the request. Aborting instead killed every
/// agent step after the first.
fn plan_kv_reuse(prefix_len: usize, truncate_ok: bool) -> KvReusePlan {
    match (prefix_len, truncate_ok) {
        (0, _) => KvReusePlan::Clear,
        (_, false) => KvReusePlan::Clear,
        (n, true) => KvReusePlan::Truncate(n),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossbeam_channel::bounded;

    #[test]
    fn kv_reuse_falls_back_to_a_clear_when_removal_is_rejected() {
        // The regression that killed every agent step after the first: llama.cpp
        // rejects a partial `kv_cache_seq_rm` once the sequence's cells are no
        // longer contiguous from its head, which is the normal state after a
        // decode+generate cycle. That must degrade to a full re-decode, never an
        // error.
        assert_eq!(plan_kv_reuse(512, false), KvReusePlan::Clear);
        // Removal succeeded: keep the prefix and truncate the stale tail.
        assert_eq!(plan_kv_reuse(512, true), KvReusePlan::Truncate(512));
        // Nothing reusable either way.
        assert_eq!(plan_kv_reuse(0, true), KvReusePlan::Clear);
        assert_eq!(plan_kv_reuse(0, false), KvReusePlan::Clear);
    }

    #[test]
    fn llama_build_info_reports_a_real_backend_snapshot() {
        // The probe reads llama.cpp's own compile-time constants; it must
        // always succeed (no backend init required) and report a plausible
        // device count and the pinned bindings version.
        let info = llama_build_info();
        assert_eq!(info.bindings_version, LLAMA_BINDINGS_VERSION);
        assert!(!info.bindings_version.is_empty());
        assert!(
            info.device_count >= 1,
            "llama.cpp must register at least a CPU backend"
        );
        assert!(info.max_devices >= 1);
        assert!(info.compute_buffer_mib > 0.0);
        // The bundled CPU-only build must not claim GPU support.
        if !cfg!(any(feature = "gpu-cuda", feature = "gpu-metal")) {
            assert!(!info.gpu_feature_enabled);
        }
    }

    #[test]
    fn common_prefix_len_stops_at_the_first_divergent_token() {
        let t = |ids: &[i32]| -> Vec<llama_cpp_2::token::LlamaToken> {
            ids.iter()
                .map(|i| llama_cpp_2::token::LlamaToken(*i))
                .collect()
        };
        // Identical sequences reuse everything.
        assert_eq!(common_prefix_len(&t(&[1, 2, 3]), &t(&[1, 2, 3])), 3);
        // A single mismatch caps reuse at that point.
        assert_eq!(common_prefix_len(&t(&[1, 2, 3]), &t(&[1, 9, 3])), 1);
        // A shorter cache caps reuse at its length.
        assert_eq!(common_prefix_len(&t(&[1, 2]), &t(&[1, 2, 3])), 2);
        // A shorter prompt never reuses beyond itself.
        assert_eq!(common_prefix_len(&t(&[1, 2, 3]), &t(&[1, 2])), 2);
        // Empty cache or empty prompt means no reuse.
        assert_eq!(common_prefix_len(&t(&[]), &t(&[1, 2])), 0);
        assert_eq!(common_prefix_len(&t(&[1, 2]), &t(&[])), 0);
    }

    #[test]
    fn load_diag_causes_take_the_earliest_concrete_lines() {
        let diag = vec![
            "gguf_init_from_reader: tensor name 4 is too long: 71 >= 64".to_string(),
            "gguf_init_from_reader: failed to read tensor info".to_string(),
            "llama_model_load: error loading model: llama_model_loader: failed to load model from /x/y.gguf"
                .to_string(),
        ];
        // The trailing line is only a summary, so the actionable cause leads.
        assert_eq!(
            load_diag_causes(&diag),
            vec![
                "tensor name 4 is too long: 71 >= 64".to_string(),
                "failed to read tensor info".to_string()
            ]
        );
    }

    #[test]
    fn load_diag_causes_strip_only_the_module_tag() {
        let diag = vec!["llama_model_load: error loading model: bad arch".to_string()];
        assert_eq!(
            load_diag_causes(&diag),
            vec!["error loading model: bad arch".to_string()]
        );
    }

    #[test]
    fn load_diag_causes_keep_a_sentence_starting_with_a_capital() {
        // A prefix is only a module tag when it is all-lowercase/underscore.
        let diag = vec!["Tensor data is missing".to_string()];
        assert_eq!(load_diag_causes(&diag), vec!["Tensor data is missing"]);
    }

    #[test]
    fn load_diag_causes_skip_duplicates_and_blanks() {
        let diag = vec![
            "gguf_init_from_reader: same".to_string(),
            "gguf_init_from_reader: same".to_string(),
            "   ".to_string(),
        ];
        assert_eq!(load_diag_causes(&diag), vec!["same".to_string()]);
    }

    #[test]
    fn load_diag_causes_are_empty_without_diagnostics() {
        assert!(load_diag_causes(&[]).is_empty());
    }

    #[test]
    fn load_hint_flags_split_model_shards() {
        for d in [
            "llama_model_load: error loading model: illegal split file idx: 2 (file: /x/-00003-of-00003.gguf), model must be loaded with the first split",
            "model must be loaded with the first split",
        ] {
            let hint = load_hint(d).unwrap_or_else(|| panic!("expected a hint for {d}"));
            assert!(hint.contains("first shard"), "hint should point at shard 1: {hint}");
        }
    }

    #[test]
    fn load_hint_flags_oversized_tensor_names() {
        let d = "tensor name 4 is too long: 71 >= 64";
        let hint = load_hint(d).expect("expected a hint");
        assert!(hint.contains("64-byte"));
    }

    #[test]
    fn load_hint_flags_truncated_downloads() {
        for d in [
            "unexpected EOF while reading tensor data",
            "failed to read tensor data",
            "file is too short",
        ] {
            let hint = load_hint(d).unwrap_or_else(|| panic!("expected a hint for {d}"));
            assert!(hint.contains("truncated") || hint.contains("shard"));
        }
    }

    #[test]
    fn load_hint_flags_unknown_architecture() {
        let hint = load_hint("unknown architecture 'foo'").expect("expected a hint");
        assert!(hint.contains("architecture"));
    }

    #[test]
    fn load_hint_is_none_for_unrecognised_failures() {
        assert!(load_hint("out of memory").is_none());
    }

    #[test]
    fn load_diag_buffer_is_bounded_and_drainable() {
        // Same lock the real load path takes, so a concurrent load cannot
        // interleave its lines into this buffer.
        let _guard = lock_load();
        take_load_diag();
        for i in 0..(LOAD_DIAG_CAP * 2) {
            push_load_diag(&format!("line {i}"));
        }
        let drained = take_load_diag();
        assert_eq!(drained.len(), LOAD_DIAG_CAP);
        // Draining empties the buffer so a later load cannot inherit it.
        assert!(take_load_diag().is_empty());
    }

    #[test]
    fn load_error_uses_native_reason_over_the_generic_null_result() {
        let diag = vec![
            "gguf_init_from_reader: tensor name 4 is too long: 71 >= 64".to_string(),
            "llama_model_load: error loading model: failed to load model".to_string(),
        ];
        let msg = format_load_error(llama_cpp_2::LlamaModelLoadError::NullResult, &diag);
        assert!(msg.contains("failed to load model"));
        assert!(!msg.contains("null result from llama cpp"));
    }

    #[test]
    fn load_error_falls_back_to_the_wrapper_message_without_diagnostics() {
        let msg = format_load_error(llama_cpp_2::LlamaModelLoadError::NullResult, &[]);
        assert!(msg.contains("null result from llama cpp"));
    }

    /// Same check for a shard of a split model: it must report the shard rule
    /// rather than the generic null pointer. Ignored by default.
    #[test]
    #[ignore = "requires a split GGUF shard on disk"]
    fn load_error_on_real_split_shard() {
        install_native_model_logs();
        let path = std::path::Path::new(
            r"D:\ai\models\Qwen--Qwen2.5-Coder-7B-Instruct-GGUF\qwen2.5-coder-7b-instruct-q8_0-00003-of-00003.gguf",
        );
        if !path.is_file() {
            panic!("sample not present: {}", path.display());
        }
        let err = match load_model_with_progress(path, &ModelInitParams::default(), |_| true) {
            Ok(_) => panic!("a non-first shard must not load"),
            Err(e) => e,
        };
        println!("USER-FACING ERROR: {err}");
        assert!(
            !err.contains("null result from llama cpp"),
            "generic message leaked"
        );
        assert!(err.contains("first shard"), "missing shard guidance: {err}");
    }

    /// End-to-end check of the user-facing message against a real unloadable
    /// GGUF. Ignored by default because it needs a specific file on disk; run
    /// with `cargo test -- --ignored load_error_on_real_bad_gguf`.
    #[test]
    #[ignore = "requires an unloadable GGUF on disk"]
    fn load_error_on_real_bad_gguf() {
        install_native_model_logs();
        let path = std::path::Path::new(
            r"D:\delete\models\hub\models--unsloth--Qwen-Image-2.1-GGUF\snapshots\2c31ccd392b367a6637841a143813320a02dff55\qwen-image-2.1-Q2_K.gguf",
        );
        if !path.is_file() {
            panic!("sample not present: {}", path.display());
        }
        let err = match load_model_with_progress(path, &ModelInitParams::default(), |_| true) {
            Ok(_) => panic!("this GGUF must not load"),
            Err(e) => e,
        };
        println!("USER-FACING ERROR: {err}");
        assert!(
            !err.contains("null result from llama cpp"),
            "generic message leaked"
        );
        assert!(err.contains("tensor name"), "missing root cause: {err}");
    }

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
            stop_words: None,
            cached_prefix_tokens: None,
            ..Default::default()
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
            || Ok(Box::new(FakeGen { tag: "pool".into() }) as Box<dyn TextGenerator>),
            ev_tx.clone(),
            1,
        )
        .expect("spawn pool");

        // Local-only registry: every role must fall back to the pool handle.
        let mut router = RuntimeRouter::new(
            &pool,
            ProviderRegistry::with_defaults("qwen", None),
            ev_tx.clone(),
        );
        for role in [
            ProviderRole::Planner,
            ProviderRole::Editor,
            ProviderRole::Autocomplete,
            ProviderRole::Embed,
        ] {
            let gen = router.resolve(role).expect("local role resolves");
            assert_eq!(
                gen.info().name,
                "pool",
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
        assert_eq!(
            router2
                .resolve(ProviderRole::Editor)
                .expect("editor routes")
                .info()
                .name,
            "pool"
        );
    }

    #[test]
    fn runtime_router_spawns_worker_pool_per_remote_role() {
        let (ev_tx, _ev_rx) = bounded::<WorkerEvent>(256);
        let pool = EnginePool::spawn_with(
            || Ok(Box::new(FakeGen { tag: "pool".into() }) as Box<dyn TextGenerator>),
            ev_tx.clone(),
            1,
        )
        .expect("spawn pool");

        // Editor role routed to a remote provider → a dedicated worker pool is
        // lazily built (no network touched at construction time). The local
        // default keeps every unrouted role on the primary pool.
        let mut reg = ProviderRegistry::with_defaults("qwen", None);
        let lm = ProviderConfig::remote(
            ProviderKind::LmStudio,
            "lm",
            "http://localhost:1234/v1",
            "",
            "code-model",
        );
        reg.add_provider(lm.clone());
        reg.set_role_provider(ProviderRole::Editor, lm.id.clone());

        let mut router = RuntimeRouter::new(&pool, reg, ev_tx);
        let gen = router
            .resolve(ProviderRole::Editor)
            .expect("editor routes to a remote worker pool");
        assert_eq!(gen.info().name, "code-model");
        assert_eq!(router.role_pool_len(ProviderRole::Editor), Some(2));

        // A second resolve for the same role hands out a fresh handle (the
        // pool is cached, not rebuilt).
        let gen2 = router
            .resolve(ProviderRole::Editor)
            .expect("second editor resolve reuses the pool");
        assert_eq!(gen2.info().name, "code-model");
        assert_eq!(router.role_pool_len(ProviderRole::Editor), Some(2));

        // An unrouted/local role still borrows the primary pool.
        let planner = router
            .resolve(ProviderRole::Planner)
            .expect("planner falls back to the primary pool");
        assert_eq!(planner.info().name, "pool");
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
            ..Default::default()
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
            stop_words: Some(vec![]),
            cached_prefix_tokens: None,
            ..Default::default()
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
            stop_words: Some(vec![]),
            cached_prefix_tokens: None,
            ..Default::default()
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
            stop_words: Some(vec![]),
            cached_prefix_tokens: None,
            ..Default::default()
        };
        let big = run_generation(&mut engine, &big_request, 1, &interrupt, &tx)
            .expect("long-prompt generation must not fail");
        eprintln!(
            "long prompt OK ({} tokens, {})",
            big.done.total_tokens, big.done.stop_reason
        );
        assert!(!big.full_text.trim().is_empty());

        // Prefix reuse: a follow-up turn that opens with the same long
        // instruction block must reuse those KV cells instead of re-evaluating
        // them, and must still produce a coherent answer. This is the case that
        // used to fail with "Decode Error -1: n_tokens == 0".
        let reuse_request = InferenceRequest {
            prompt: format!(
                "Here is a long instruction list for you. {}\nNow answer: what is 3+3?",
                "A numbered fact: 42. ".repeat(130)
            ),
            messages: None,
            images: None,
            max_tokens: 24,
            stop_words: Some(vec![]),
            cached_prefix_tokens: None,
            ..Default::default()
        };
        let reused = run_generation(&mut engine, &reuse_request, 1, &interrupt, &tx)
            .expect("prefix-reuse generation must not fail");
        eprintln!(
            "prefix reuse: input={} cache_read={} cache_write={} tokens={} stop={}",
            reused.done.input_tokens,
            reused.done.cache_read_tokens,
            reused.done.cache_write_tokens,
            reused.done.total_tokens,
            reused.done.stop_reason
        );
        assert!(
            reused.done.cache_read_tokens > 0,
            "expected KV cache hits on a near-identical follow-up prompt, got {}",
            reused.done.cache_read_tokens
        );
        assert!(!reused.full_text.trim().is_empty());

        // A third turn with a *different* opening must fall back to a full
        // re-evaluate rather than reusing a prefix that is not there.
        let diverge = InferenceRequest {
            prompt: "Completely unrelated question: what is the capital of France?".to_string(),
            messages: None,
            images: None,
            max_tokens: 24,
            stop_words: Some(vec![]),
            cached_prefix_tokens: None,
            ..Default::default()
        };
        let diverged = run_generation(&mut engine, &diverge, 1, &interrupt, &tx)
            .expect("divergent generation must not fail");
        eprintln!(
            "divergent: input={} cache_read={} tokens={}",
            diverged.done.input_tokens, diverged.done.cache_read_tokens, diverged.done.total_tokens
        );
        assert_eq!(
            diverged.done.cache_read_tokens, 0,
            "no prefix should be reused for an unrelated prompt"
        );
        assert!(!diverged.full_text.trim().is_empty());
    }

    /// Throughput probe: decode the same prompt at several thread counts and
    /// print tokens/sec, so a change to the build's CPU features or the pool's
    /// thread split can be measured instead of guessed at. Skipped without the
    /// model file; run with `cargo test -- --ignored`.
    #[test]
    #[ignore = "requires the real GGUF model file on disk"]
    fn bench_decode_tokens_per_sec_by_thread_count() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../models/qwen2.5-0.5b-instruct-q4_k_m.gguf");
        if !path.is_file() {
            eprintln!("SKIP: model not found at {}", path.display());
            return;
        }
        let params = ModelInitParams {
            context_size: Some(2048),
            n_gpu_layers: Some(0),
            ..Default::default()
        };
        let model = load_model_with_progress(&path, &params, |_| true).expect("load model");
        let (tx, _rx) = bounded::<WorkerEvent>(4096);
        let interrupt = CancellationToken::new();
        let request = InferenceRequest {
            prompt: "Explain what a hash map is in detail.".to_string(),
            messages: None,
            images: None,
            max_tokens: 96,
            stop_words: Some(vec![]),
            cached_prefix_tokens: None,
            ..Default::default()
        };
        for threads in [4u32, 8] {
            let mut engine = model
                .new_engine_with_threads(threads as i32)
                .expect("build engine");
            // Warm up so the first (cold) run does not skew the number.
            let _ = run_generation(&mut engine, &request, 1, &interrupt, &tx);
            let out = run_generation(&mut engine, &request, 1, &interrupt, &tx)
                .expect("generation must not fail");
            eprintln!(
                "BENCH threads={threads} tokens={} tok/s={:.1} elapsed_ms={}",
                out.done.total_tokens, out.done.tokens_per_sec, out.done.elapsed_ms
            );
        }
    }

    // -----------------------------------------------------------------------
    // ModelInitParams — wire compat, merging, validation, llama.cpp args
    // -----------------------------------------------------------------------

    #[test]
    fn model_init_params_wire_roundtrip() {
        let cfg = ModelInitParams {
            n_gpu_layers: Some(29),
            context_size: Some(8192),
            n_threads: Some(3),
            n_workers: Some(4),
            n_batch: Some(2048),
            n_ubatch: Some(512),
            unified_kv: Some(true),
            flash_attn: Some(true),
            offload_kqv: Some(false),
            use_mmap: Some(true),
            use_mlock: Some(false),
            rope_freq_base: Some(1_000_000.0),
            rope_freq_scale: Some(1.0),
            cache_type_k: Some("Q8_0".into()),
            cache_type_v: Some("Q8_0".into()),
            seed: Some(1234),
            speculative: None,
            context_checkpoints: Some(32),
            chat_template: Some("chatml".into()),
            reasoning_budget: Some("prefer short answers".into()),
            llama_args: Some("--no-mmap".into()),
            ..Default::default()
        };
        let json = serde_json::to_string(&cfg).expect("serialize");
        // camelCase over the wire + options preserved.
        assert!(json.contains("\"nGpuLayers\":29"), "{json}");
        assert!(json.contains("\"cacheTypeK\":\"Q8_0\""), "{json}");
        assert!(json.contains("\"reasoningBudget\""), "{json}");
        let back: ModelInitParams = serde_json::from_str(&json).expect("round-trip");
        assert_eq!(back.n_gpu_layers, Some(29));
        assert_eq!(back.context_size, Some(8192));
        assert_eq!(back.cache_type_k.as_deref(), Some("Q8_0"));
        assert_eq!(
            back.reasoning_budget.as_deref(),
            Some("prefer short answers")
        );
        assert_eq!(back.llama_args.as_deref(), Some("--no-mmap"));

        // An old payload without the newer fields must still parse to `None`
        // (backward compatibility — old settings.json / old frontends).
        let old: ModelInitParams =
            serde_json::from_str(r#"{"contextSize":4096}"#).expect("legacy subset parses");
        assert_eq!(old.context_size, Some(4096));
        assert!(old.n_batch.is_none());
        assert!(old.reasoning_budget.is_none());
    }

    #[test]
    fn model_init_params_merge_precedence() {
        let base = ModelInitParams {
            context_size: Some(1024),
            n_gpu_layers: Some(0),
            n_threads: Some(2),
            n_workers: Some(1),
            ..Default::default()
        };
        let explicit = ModelInitParams {
            context_size: Some(8192),
            cache_type_k: Some("Q4_0".into()),
            llama_args: Some("--no-mmap".into()),
            ..Default::default()
        };
        let merged = base.merge(&explicit);
        assert_eq!(merged.context_size, Some(8192), "explicit wins");
        assert_eq!(
            merged.n_gpu_layers,
            Some(0),
            "base kept where explicit is None"
        );
        assert_eq!(merged.cache_type_k.as_deref(), Some("Q4_0"));
        assert_eq!(merged.n_threads, Some(2));
        assert!(merged.unified_kv.is_none());
    }

    #[test]
    fn model_init_params_validate_ranges() {
        let ok = ModelInitParams {
            n_threads: Some(4),
            n_workers: Some(4),
            n_batch: Some(512),
            n_ubatch: Some(256),
            rope_freq_base: Some(10_000.0),
            cache_type_k: Some("Q8_0".into()),
            seed: Some(42),
            speculative: Some("off".into()),
            ..Default::default()
        };
        assert!(ok.validate().is_ok());

        for bad in [
            ModelInitParams {
                n_threads: Some(0),
                ..Default::default()
            },
            ModelInitParams {
                n_workers: Some(33),
                ..Default::default()
            },
            ModelInitParams {
                n_batch: Some(0),
                ..Default::default()
            },
            ModelInitParams {
                n_gpu_layers: Some(20_000),
                ..Default::default()
            },
            ModelInitParams {
                rope_freq_base: Some(0.0),
                ..Default::default()
            },
            ModelInitParams {
                cache_type_k: Some("BRPF".into()),
                ..Default::default()
            },
            ModelInitParams {
                seed: Some(u32::MAX),
                ..Default::default()
            },
            ModelInitParams {
                speculative: Some("mtp".into()),
                ..Default::default()
            },
        ] {
            assert!(bad.validate().is_err(), "expected error for {bad:?}");
        }
        // ubatch cannot exceed batch.
        let ubatch_big = ModelInitParams {
            n_batch: Some(128),
            n_ubatch: Some(256),
            ..Default::default()
        };
        assert!(ubatch_big.validate().is_err());
    }

    #[test]
    fn parse_cache_type_cases() {
        assert!(matches!(parse_cache_type("Q8_0"), Some(KvCacheType::Q8_0)));
        assert!(matches!(parse_cache_type("q4_0"), Some(KvCacheType::Q4_0)));
        assert!(matches!(parse_cache_type("F16"), Some(KvCacheType::F16)));
        assert!(parse_cache_type("WAT").is_none());
    }

    #[test]
    fn apply_llama_args_mapping_and_errors() {
        let mut p = ModelInitParams {
            context_size: Some(1024),
            ..Default::default()
        };
        p.apply_llama_args("--ctx-size 4096 --threads 6 --gpu-layers 24 --batch-size 1024 --ubatch-size 128 --parallel 4 --seed 7 --rope-freq-base=1000000 --cache-type-k Q8_0 --flash-attn --mlock")
            .expect("valid args parse");
        assert_eq!(p.context_size, Some(4096), "flag overrides UI value");
        assert_eq!(p.n_threads, Some(6));
        assert_eq!(p.n_gpu_layers, Some(24));
        assert_eq!(p.n_batch, Some(1024));
        assert_eq!(p.n_ubatch, Some(128));
        assert_eq!(p.n_workers, Some(4));
        assert_eq!(p.seed, Some(7));
        assert_eq!(p.rope_freq_base, Some(1_000_000.0));
        assert_eq!(p.cache_type_k.as_deref(), Some("Q8_0"));
        assert_eq!(p.flash_attn, Some(true));
        assert_eq!(p.use_mlock, Some(true));

        let unknown = ModelInitParams::default().apply_llama_args("--nonsense 3");
        assert!(unknown.is_err());
        assert!(unknown.unwrap_err().contains("--nonsense"));

        let missing_value = ModelInitParams::default().apply_llama_args("--ctx-size");
        assert!(missing_value.is_err());
        assert!(missing_value.unwrap_err().contains("requires a value"));

        assert!(ModelInitParams::default()
            .apply_llama_args("--no-mmap --no-flash-attn --cache-type-v F16")
            .is_ok());
    }

    #[test]
    fn flash_policy_mapping() {
        assert_eq!(
            LoadedModel::flash_policy(Some(true)),
            LLAMA_FLASH_ATTN_TYPE_ENABLED
        );
        assert_eq!(
            LoadedModel::flash_policy(Some(false)),
            LLAMA_FLASH_ATTN_TYPE_DISABLED
        );
        assert_eq!(LoadedModel::flash_policy(None), LLAMA_FLASH_ATTN_TYPE_AUTO);
    }
}
