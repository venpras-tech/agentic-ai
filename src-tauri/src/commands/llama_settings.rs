//! llama.cpp bindings update probes and persisted media-engine paths for the
//! Settings → llama.cpp page.

use serde::{Deserialize, Serialize};
use tauri::AppHandle;

use crate::app_data_dir;
use crate::engine::LLAMA_BINDINGS_VERSION;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LlamaBindingsUpdateInfo {
    pub current_version: String,
    pub latest_version: String,
    pub update_available: bool,
    pub crates_io_url: String,
    pub upstream_repo_url: String,
    pub note: String,
}

/// Compare the pinned `llama-cpp-2` crate against crates.io. Updating the
/// in-process engine requires rebuilding the desktop app with the newer
/// bindings; this command only reports availability and documents next steps.
#[tauri::command]
pub async fn llama_bindings_update_check() -> Result<LlamaBindingsUpdateInfo, String> {
    let current = LLAMA_BINDINGS_VERSION.to_string();
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .map_err(|e| e.to_string())?;
    let resp = client
        .get("https://crates.io/api/v1/crates/llama-cpp-2")
        .header("User-Agent", "ai-editor/0.1 (llama-bindings-check)")
        .send()
        .await
        .map_err(|e| format!("Could not reach crates.io: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("crates.io returned HTTP {}", resp.status()));
    }
    let body: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    let latest = body
        .pointer("/crate/max_version")
        .and_then(|v| v.as_str())
        .unwrap_or(&current)
        .to_string();
    let update_available = version_gt(&latest, &current);
    let note = if update_available {
        "A newer llama-cpp-2 crate is published. Rebuild the desktop app with that version \
         (or install an app release that bundles it) to run the newer llama.cpp inside the editor. \
         Hot-swapping the engine at runtime is not supported."
            .into()
    } else {
        "You are on the newest published llama-cpp-2 bindings this check knows about.".into()
    };
    Ok(LlamaBindingsUpdateInfo {
        current_version: current,
        latest_version: latest,
        update_available,
        crates_io_url: "https://crates.io/crates/llama-cpp-2".into(),
        upstream_repo_url: "https://github.com/ggml-org/llama.cpp".into(),
        note,
    })
}

fn version_gt(a: &str, b: &str) -> bool {
    let pa = parse_version_triplet(a);
    let pb = parse_version_triplet(b);
    pa > pb
}

fn parse_version_triplet(s: &str) -> (u32, u32, u32) {
    let mut it = s.split('.');
    let a = it.next().unwrap_or("0").parse().unwrap_or(0);
    let b = it.next().unwrap_or("0").parse().unwrap_or(0);
    let c = it.next().unwrap_or("0").parse().unwrap_or(0);
    (a, b, c)
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaEngineSettings {
    pub local_binary: Option<String>,
    pub image_model_path: Option<String>,
    pub video_model_path: Option<String>,
}

fn media_settings_path(app: &AppHandle) -> std::path::PathBuf {
    app_data_dir(app).join("media-engine.json")
}

pub(crate) fn load_media_settings(app: &AppHandle) -> MediaEngineSettings {
    let path = media_settings_path(app);
    match std::fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
        Err(_) => MediaEngineSettings::default(),
    }
}

#[tauri::command]
pub async fn media_engine_settings_load(app: AppHandle) -> Result<MediaEngineSettings, String> {
    Ok(load_media_settings(&app))
}

#[tauri::command]
pub async fn media_engine_settings_save(
    app: AppHandle,
    settings: MediaEngineSettings,
) -> Result<(), String> {
    let path = media_settings_path(&app);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let text = serde_json::to_string_pretty(&settings).map_err(|e| e.to_string())?;
    std::fs::write(&path, text).map_err(|e| e.to_string())?;
    Ok(())
}
