//! Persisted settings (`{app_data}/settings.json`): the typed shape, the
//! lenient parser, the model-path persistence helper and the settings IPC.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde_json::Value;
use tauri::AppHandle;

use crate::engine::ModelInitParams;
use crate::remote::RemoteModelConfig;
use crate::app_data_dir;

/// Key under which the global ("no specific model") local-load config is
/// stored in the `localModelConfigs` map.
pub(crate) const DEFAULT_LOCAL_CONFIG_KEY: &str = "default";

/// Generation defaults persisted under the settings `params` key (camelCase,
/// matching the frontend `GenParams` shape). Opt-in later adds of load-only
/// fields (e.g. `nWorkers`) simply land in [`AppSettings`]'s catch-all.
#[derive(serde::Serialize, serde::Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
struct GenParamsSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    context_size: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    n_threads: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    n_gpu_layers: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    seed: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    max_tokens: Option<i32>,
}

/// How the built-in OpenAI-compatible HTTP server behaves when a local model
/// is loaded. Persisted under the settings `localApiServer` key (camelCase,
/// mirroring the Settings → LLM card / `AppSettings.local_api_server`).
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct LocalApiServerSettings {
    /// Start serving on `127.0.0.1:{port}/v1` automatically whenever a local
    /// model finishes loading.
    #[serde(default = "default_local_api_enabled")]
    pub enabled: bool,
    /// Loopback port the server listens on.
    #[serde(default = "default_local_api_port")]
    pub port: u16,
    /// Route the in-app assistant through the loopback server (OpenAI-compatible
    /// client) instead of direct native-pool calls, mirroring how Ollama and
    /// LM Studio connect. Output text is unchanged.
    #[serde(default = "default_local_api_route")]
    pub route_assistant: bool,
}

const fn default_local_api_enabled() -> bool {
    true
}

const fn default_local_api_port() -> u16 {
    8080
}

const fn default_local_api_route() -> bool {
    true
}

impl Default for LocalApiServerSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            port: 8080,
            route_assistant: true,
        }
    }
}

/// Typed shape of `{app_data}/settings.json`, written by both the model
/// lifecycle (`modelPath`) and the frontend (params / remote / workspace
/// pointers). Unknown keys survive a load→save round-trip via the flatten
/// catch-all so the config file format never breaks for newer/older writers.
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppSettings {
    /// Last successfully loaded GGUF path (auto-restored on next launch).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) model_path: Option<String>,
    /// Recently loaded local GGUF paths (frontend MRU list).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    recent_models: Option<Vec<String>>,
    /// Generation defaults (`GenParams`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    params: Option<GenParamsSettings>,
    /// Last-used remote endpoint config.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    remote: Option<RemoteModelConfig>,
    /// Built-in local OpenAI-compatible server (auto-start on model load).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_api_server: Option<LocalApiServerSettings>,
    /// Last workspace root (hydrated on startup).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_workspace: Option<String>,
    /// All workspace roots (multi-root).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_workspaces: Option<Vec<String>>,
    /// Last active chat pointer `{project, chatId}`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_chat: Option<LastChatPointer>,
    /// Per-model local-load configurations keyed by GGUF path. The `"default"`
    /// key holds the fallback applied when a model has no entry of its own.
    /// Every value is a [`ModelInitParams`] (all fields optional).
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    local_model_configs: HashMap<String, ModelInitParams>,
    /// Any other keys (keep the file format backwards-compatible).
    #[serde(flatten)]
    extra: std::collections::HashMap<String, Value>,
}

/// Persisted `{project, chatId}` pointer — the chat reopened on the last
/// launch (mirrors `src/types.ts` `LastChatPointer`).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct LastChatPointer {
    project: String,
    #[serde(default)]
    chat_id: Option<String>,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            model_path: None,
            recent_models: None,
            params: None,
            remote: None,
            local_api_server: None,
            last_workspace: None,
            last_workspaces: None,
            last_chat: None,
            local_model_configs: HashMap::new(),
            extra: std::collections::HashMap::new(),
        }
    }
}

/// Parse a settings file leniently: typed fields are honoured when they match,
/// and anything that cannot be forced into the typed shape (legacy keys with
/// unexpected value types, unknown keys, …) is preserved in the catch-all
/// instead of failing the whole load.
pub(crate) fn parse_settings(text: &str) -> AppSettings {
    let raw: Value = serde_json::from_str(text).unwrap_or(Value::Object(Default::default()));
    match serde_json::from_value::<AppSettings>(raw.clone()) {
        Ok(s) => s,
        Err(_) => {
            let mut s = AppSettings::default();
            if let Value::Object(mut map) = raw {
                // modelPath drives model auto-load; keep it alive even when a
                // sibling field is corrupt enough to break the typed parse.
                if let Some(v) = map.remove("modelPath") {
                    if let Ok(p) = serde_json::from_value::<String>(v.clone()) {
                        s.model_path = Some(p);
                    } else {
                        map.insert("modelPath".into(), v);
                    }
                }
                s.extra = map.into_iter().collect();
            }
            s
        }
    }
}

impl AppSettings {
    /// Model-loading params persisted by the Settings → LLM page, mapped onto
    /// [`ModelInitParams`] for `install_local_model`. Load-only fields like
    /// `nWorkers` live in the catch-all (`settings.json` top level), the rest
    /// under the `params` key. This is the *legacy* source: per-model configs
    /// (see [`AppSettings::merge_init_params`]) take precedence over it.
    pub(crate) fn model_init_params(&self) -> ModelInitParams {
        let p = self.params.clone();
        let n_workers = self
            .extra
            .get("nWorkers")
            .and_then(|v| v.as_u64())
            .map(|v| v as u32);
        ModelInitParams {
            n_gpu_layers: p.as_ref().and_then(|g| g.n_gpu_layers),
            context_size: p.as_ref().and_then(|g| g.context_size),
            n_threads: p.as_ref().and_then(|g| g.n_threads),
            n_workers,
            seed: p.as_ref().and_then(|g| g.seed),
            max_tokens: p.as_ref().and_then(|g| g.max_tokens),
            ..Default::default()
        }
    }

    /// The persisted config for one model path (exact string match), falling
    /// back to the `"default"` entry — then `None`.
    pub(crate) fn local_config(&self, path: Option<&str>) -> Option<&ModelInitParams> {
        self.local_model_configs
            .get(path.unwrap_or_default())
            .or_else(|| self.local_model_configs.get(DEFAULT_LOCAL_CONFIG_KEY))
    }

    /// Resolve the full config used to load `path`, in precedence order:
    /// legacy `params`/`nWorkers` settings ← `"default"` config ← per-path
    /// config ← fields explicitly supplied by the load caller. Every layer is
    /// merged field-by-field, so an old config that only ever set
    /// `contextSize` keeps working while newer per-model values win where both
    /// exist.
    pub(crate) fn merge_init_params(
        &self,
        path: Option<&str>,
        explicit: &ModelInitParams,
    ) -> ModelInitParams {
        let mut out = self.model_init_params();
        if let Some(cfg) = self.local_config(path) {
            out = out.merge(cfg);
        }
        out.merge(explicit)
    }

    /// Store a config for `path` (`None`/`"default"` = global fallback) and
    /// return the updated settings.
    pub(crate) fn persist_local_config(
        &mut self,
        path: Option<&str>,
        config: &ModelInitParams,
    ) {
        let key = path.filter(|p| !p.is_empty()).unwrap_or(DEFAULT_LOCAL_CONFIG_KEY).to_string();
        self.local_model_configs.insert(key, config.clone());
    }

    /// Built-in server behaviour (enabled by default, port 8080).
    pub(crate) fn local_api_server(&self) -> LocalApiServerSettings {
        self.local_api_server
            .clone()
            .unwrap_or_default()
    }
}

/// Load `settings.json` from disk, or a default when absent/corrupt.
pub(crate) fn read_settings(app: &AppHandle) -> AppSettings {
    let text =
        std::fs::read_to_string(app_data_dir(app).join("settings.json")).ok();
    text.as_deref().map(parse_settings).unwrap_or_default()
}

/// Best-effort write of `settings.json` (a failure must never fail the
/// caller — e.g. a model load — so it is swallowed).
async fn write_settings_best_effort(app: &AppHandle, settings: &AppSettings) {
    let dir = app_data_dir(app);
    if tokio::fs::create_dir_all(&dir).await.is_err() {
        return;
    }
    if let Ok(text) = serde_json::to_string_pretty(settings) {
        let _ = tokio::fs::write(dir.join("settings.json"), text).await;
    }
}

#[tauri::command]
pub async fn local_model_config_save(
    app: AppHandle,
    path: Option<String>,
    config: ModelInitParams,
) -> Result<(), String> {
    let dir = app_data_dir(&app);
    let file = dir.join("settings.json");
    let mut settings: AppSettings = tokio::fs::read_to_string(&file)
        .await
        .ok()
        .as_deref()
        .map(parse_settings)
        .unwrap_or_default();
    let key = path
        .as_deref()
        .filter(|p| !p.is_empty())
        .unwrap_or(DEFAULT_LOCAL_CONFIG_KEY);
    settings.persist_local_config(Some(key), &config);
    write_settings_best_effort(&app, &settings).await;
    Ok(())
}

#[tauri::command]
pub async fn local_model_configs_load(app: AppHandle) -> Result<HashMap<String, ModelInitParams>, String> {
    Ok(read_settings(&app).local_model_configs)
}

/// Resolve the per-model config for `path` (persisted `"default"` → per-path →
/// explicit) and apply it to the current `settings.json`. Used right before a
/// model load so the freshly-picked values become the persisted baseline for
/// that path (`None` = global default).
pub(crate) async fn resolve_and_persist(
    app: &AppHandle,
    path: &str,
    explicit: &ModelInitParams,
) -> ModelInitParams {
    let dir = app_data_dir(app);
    let file = dir.join("settings.json");
    let mut settings: AppSettings = tokio::fs::read_to_string(&file)
        .await
        .ok()
        .as_deref()
        .map(parse_settings)
        .unwrap_or_default();
    let resolved = settings.merge_init_params(Some(path), explicit);
    settings.persist_local_config(Some(path), &resolved);
    write_settings_best_effort(app, &settings).await;
    resolved
}

/// Remember the last successfully loaded GGUF so future launches can restore
/// it without re-picking (read-modify-write of `settings.json`; best-effort —
/// a failure must never fail the model load itself).
pub(crate) async fn persist_model_path(app: &AppHandle, path: &Path) {
    let dir = app_data_dir(app);
    let file = dir.join("settings.json");
    let mut settings: AppSettings = tokio::fs::read_to_string(&file)
        .await
        .ok()
        .as_deref()
        .map(parse_settings)
        .unwrap_or_default();
    let as_str = path.display().to_string();
    if settings.model_path.as_deref() == Some(as_str.as_str()) {
        return;
    }
    settings.model_path = Some(as_str);
    if tokio::fs::create_dir_all(&dir).await.is_ok() {
        if let Ok(text) = serde_json::to_string_pretty(&settings) {
            if tokio::fs::write(&file, text).await.is_ok() {
                crate::logging::info(None, "model", "saved default model path");
            }
        }
    }
}

#[tauri::command]
pub async fn settings_load(app: AppHandle) -> Result<AppSettings, String> {
    let dir = app_data_dir(&app);
    let path = dir.join("settings.json");
    match tokio::fs::read_to_string(&path).await {
        Ok(text) => Ok(parse_settings(&text)),
        Err(_) => Ok(AppSettings::default()),
    }
}

#[tauri::command]
pub async fn settings_save(app: AppHandle, settings: AppSettings) -> Result<(), String> {
    let dir = app_data_dir(&app);
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| e.to_string())?;
    let text = serde_json::to_string_pretty(&settings).map_err(|e| e.to_string())?;
    tokio::fs::write(dir.join("settings.json"), text)
        .await
        .map_err(|e| e.to_string())
}

/// The `modelsDir` setting (top-level, catch-all key), when the user has set a
/// non-empty custom folder for downloaded GGUFs.
pub(crate) fn configured_models_dir(app: &AppHandle) -> Option<PathBuf> {
    let text = std::fs::read_to_string(app_data_dir(app).join("settings.json")).ok()?;
    parse_settings(&text)
        .extra
        .get("modelsDir")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
}

/// Directory holding downloaded GGUFs: the `modelsDir` setting when present,
/// otherwise the default `{app_data}/models`.
pub(crate) fn models_dir(app: &AppHandle) -> PathBuf {
    configured_models_dir(app).unwrap_or_else(|| app_data_dir(app).join("models"))
}

/// Resolved vs. default models directory, surfaced in Settings → General.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelsDirInfo {
    pub models_dir: String,
    pub default_dir: String,
    pub custom: bool,
}

#[tauri::command]
pub async fn models_dir_info(app: AppHandle) -> Result<ModelsDirInfo, String> {
    Ok(ModelsDirInfo {
        models_dir: models_dir(&app).to_string_lossy().into_owned(),
        default_dir: app_data_dir(&app)
            .join("models")
            .to_string_lossy()
            .into_owned(),
        custom: configured_models_dir(&app).is_some(),
    })
}

/// Open the models folder in the OS file explorer.
///
/// Resolves through [`models_dir`] so it always opens whatever the
/// `modelsDir` setting currently points at, not a hardcoded default. The
/// directory is created first when missing, otherwise the explorer would
/// either error or silently fall back to showing the parent.
#[tauri::command]
pub async fn open_models_dir(app: AppHandle) -> Result<String, String> {
    use tauri_plugin_opener::OpenerExt;

    let dir = models_dir(&app);
    if !dir.is_dir() {
        tokio::fs::create_dir_all(&dir)
            .await
            .map_err(|e| format!("Could not create {}: {e}", dir.display()))?;
    }
    let path = dir.to_string_lossy().into_owned();
    app.opener()
        .open_path(path.clone(), None::<&str>)
        .map_err(|e| format!("Could not open {}: {e}", dir.display()))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(context_size: u32, n_gpu_layers: u32) -> ModelInitParams {
        ModelInitParams {
            context_size: Some(context_size),
            n_gpu_layers: Some(n_gpu_layers),
            ..Default::default()
        }
    }

    /// Old `settings.json` (legacy `params` + top-level `nWorkers`, no
    /// `localModelConfigs`) must still resolve to the legacy load params.
    #[test]
    fn legacy_settings_backward_compat() {
        let s = parse_settings(
            r#"{"modelPath":"D:\\models\\m.gguf","nWorkers":3,"params":{"contextSize":8192,"nThreads":4,"nGpuLayers":10,"temperature":0.1}}"#,
        );
        assert_eq!(s.model_path.as_deref(), Some("D:\\models\\m.gguf"));
        let p = s.model_init_params();
        assert_eq!(p.context_size, Some(8192));
        assert_eq!(p.n_threads, Some(4));
        assert_eq!(p.n_gpu_layers, Some(10));
        assert_eq!(p.n_workers, Some(3));
        // New fields default to None in legacy files.
        assert!(p.n_batch.is_none() && p.cache_type_k.is_none());
    }

    /// Resolution precedence: legacy < "default" config < per-path config <
    /// explicit load call.
    #[test]
    fn merge_init_params_precedence() {
        let mut s = AppSettings::default();
        s.params = Some(GenParamsSettings {
            context_size: Some(1024),
            n_threads: Some(2),
            ..Default::default()
        });
        s.local_model_configs
            .insert(DEFAULT_LOCAL_CONFIG_KEY.into(), cfg(4096, 0));
        s.local_model_configs
            .insert("D:\\models\\m.gguf".into(), cfg(8192, 16));

        let explicit = ModelInitParams {
            n_gpu_layers: Some(24),
            ..Default::default()
        };
        let resolved = s.merge_init_params(Some("D:\\models\\m.gguf"), &explicit);
        assert_eq!(resolved.context_size, Some(8192), "per-path beats default");
        assert_eq!(resolved.n_gpu_layers, Some(24), "explicit wins");
        assert_eq!(resolved.n_threads, Some(2), "legacy kept where nothing overrides");

        // A model WITHOUT its own entry falls back to the default config.
        let fallback = s.merge_init_params(Some("D:\\models\\other.gguf"), &ModelInitParams::default());
        assert_eq!(fallback.context_size, Some(4096), "default config applies");

        // No path at all → default config.
        let global = s.merge_init_params(None, &ModelInitParams::default());
        assert_eq!(global.context_size, Some(4096));
    }

    /// `localModelConfigs` survives a JSON round-trip with camelCase keys and
    /// all-optional values.
    #[test]
    fn local_configs_roundtrip() {
        let text = r#"{
            "modelPath": "D:\\models\\m.gguf",
            "localModelConfigs": {
                "default": {"contextSize": 4096, "nBatch": 512},
                "D:\\models\\m.gguf": {"contextSize": 8192, "nGpuLayers": 20, "flashAttn": true, "cacheTypeK": "Q8_0", "seed": 7}
            }
        }"#;
        let s = parse_settings(text);
        let m = s
            .local_config(Some("D:\\models\\m.gguf"))
            .expect("path config present");
        assert_eq!(m.context_size, Some(8192));
        assert_eq!(m.n_gpu_layers, Some(20));
        assert_eq!(m.flash_attn, Some(true));
        assert_eq!(m.cache_type_k.as_deref(), Some("Q8_0"));
        assert_eq!(m.seed, Some(7));
        assert_eq!(m.n_threads, None, "unset advanced field stays None");

        let serialized = serde_json::to_string(&s).expect("serialize");
        let reparsed = parse_settings(&serialized);
        assert_eq!(
            reparsed
                .local_config(Some("D:\\models\\m.gguf"))
                .and_then(|c| c.context_size),
            Some(8192),
            "round-trip keeps per-model config"
        );
        assert!(serialized.contains("localModelConfigs"));
    }

    /// `persist_local_config` writes under the path key, or `"default"` when
    /// the path is empty.
    #[test]
    fn persist_local_config_keys() {
        let mut s = AppSettings::default();
        s.persist_local_config(Some("D:\\models\\m.gguf"), &cfg(2048, 0));
        assert_eq!(s.local_config(Some("D:\\models\\m.gguf")).and_then(|c| c.context_size), Some(2048));
        s.persist_local_config(Some(""), &cfg(1024, 0));
        assert_eq!(
            s.local_model_configs.get(DEFAULT_LOCAL_CONFIG_KEY).and_then(|c| c.context_size),
            Some(1024)
        );
    }

    /// `localApiServer` defaults to enabled on port 8080 with assistant routing,
    /// survives legacy files (absent key), and round-trips camelCase.
    #[test]
    fn local_api_server_defaults_and_roundtrip() {
        let s = parse_settings(r#"{"modelPath":"D:\\models\\m.gguf"}"#);
        let las = s.local_api_server();
        assert!(las.enabled, "modern default is enabled");
        assert_eq!(las.port, 8080);
        assert!(las.route_assistant, "assistant routing defaults on");

        let text = r#"{"localApiServer":{"enabled":false,"port":9090,"routeAssistant":false}}"#;
        let las = parse_settings(text).local_api_server();
        assert!(!las.enabled);
        assert_eq!(las.port, 9090);
        assert!(!las.route_assistant);

        let serialized = serde_json::to_string(&parse_settings(text)).expect("serialize");
        assert!(serialized.contains("localApiServer"));
        assert!(serialized.contains("routeAssistant"));
        let reparsed = parse_settings(&serialized).local_api_server();
        assert_eq!(reparsed.port, 9090);
        assert!(!reparsed.enabled);
    }
}