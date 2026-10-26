//! Image / video generation (cloud OpenAI-compatible + local engine hooks).

use std::path::PathBuf;

use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use serde::{Deserialize, Serialize};
use tauri::AppHandle;

use crate::app_data_dir;
use crate::commands::llama_settings::load_media_settings;
use crate::remote::RemoteModelConfig;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaEngineStatus {
    pub cloud_ready: bool,
    pub local_binary_configured: bool,
    pub local_binary_exists: bool,
    pub image_model_configured: bool,
    pub video_model_configured: bool,
    pub message: String,
}

fn load_active_remote(app: &AppHandle) -> Option<RemoteModelConfig> {
    let path = app_data_dir(app).join("active-remote.json");
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

#[tauri::command]
pub async fn media_engine_status(app: AppHandle) -> Result<MediaEngineStatus, String> {
    let settings = load_media_settings(&app);
    let remote = load_active_remote(&app);
    let cloud_ready = remote
        .as_ref()
        .map(|r| !r.api_key.trim().is_empty() && !r.base_url.trim().is_empty())
        .unwrap_or(false);
    let local_binary_configured = settings
        .local_binary
        .as_ref()
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false);
    let local_binary_exists = settings
        .local_binary
        .as_ref()
        .map(|p| std::path::Path::new(p).is_file())
        .unwrap_or(false);
    let image_model_configured = settings
        .image_model_path
        .as_ref()
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false);
    let video_model_configured = settings
        .video_model_path
        .as_ref()
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false);
    let message = if cloud_ready {
        "Cloud image generation is available via the active remote provider.".into()
    } else if local_binary_exists {
        "Local media binary is configured. Image/video model paths can be set under Settings → llama.cpp → Media engine.".into()
    } else {
        "Connect a cloud provider with an API key (Agents) for image generation, or configure a local media engine binary in Settings.".into()
    };
    Ok(MediaEngineStatus {
        cloud_ready,
        local_binary_configured,
        local_binary_exists,
        image_model_configured,
        video_model_configured,
        message,
    })
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaGenerateImageRequest {
    pub prompt: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub model: Option<String>,
    /// `cloud` (default) or `local` (requires configured engine — not fully wired yet).
    pub mode: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaGenerateImageResult {
    pub saved_path: String,
    pub provider: String,
    pub revised_prompt: Option<String>,
}

#[tauri::command]
pub async fn media_generate_image(
    app: AppHandle,
    request: MediaGenerateImageRequest,
) -> Result<MediaGenerateImageResult, String> {
    let prompt = request.prompt.trim();
    if prompt.is_empty() {
        return Err("Prompt is required.".into());
    }
    let mode = request.mode.as_deref().unwrap_or("cloud");
    if mode == "local" {
        return Err(
            "Local image generation requires a media engine install (Wan/Flux/SD checkpoints). \
             Configure paths in Settings → llama.cpp → Media engine, or use Cloud mode with an API provider."
                .into(),
        );
    }
    let remote = load_active_remote(&app).ok_or_else(|| {
        "No active cloud provider — connect one under Agents and load a remote model.".to_string()
    })?;
    if remote.api_key.trim().is_empty() {
        return Err("Active provider has no API key.".into());
    }
    let base = remote.base_url.trim_end_matches('/');
    let url = format!("{base}/images/generations");
    let model = request
        .model
        .filter(|m| !m.trim().is_empty())
        .unwrap_or_else(|| {
            if remote.provider.eq_ignore_ascii_case("openai") {
                "gpt-image-1".to_string()
            } else {
                remote.model.clone()
            }
        });
    let size = match (request.width, request.height) {
        (Some(w), Some(h)) if w > 0 && h > 0 => format!("{}x{}", w, h),
        _ => "1024x1024".into(),
    };
    let body = serde_json::json!({
        "model": model,
        "prompt": prompt,
        "size": size,
        "n": 1,
    });
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(300))
        .build()
        .map_err(|e| e.to_string())?;
    let resp = client
        .post(&url)
        .bearer_auth(remote.api_key.trim())
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("Image request failed: {e}"))?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        let snippet: String = text.chars().take(400).collect();
        return Err(format!("Provider returned HTTP {}: {}", status, snippet));
    }
    let json: serde_json::Value = serde_json::from_str(&text).map_err(|e| format!("Bad JSON: {e}"))?;
    let item = json
        .get("data")
        .and_then(|d| d.as_array())
        .and_then(|a| a.first())
        .ok_or_else(|| "Provider response had no image data.".to_string())?;
    let revised = item
        .get("revised_prompt")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let bytes = if let Some(b64) = item.get("b64_json").and_then(|v| v.as_str()) {
        B64.decode(b64).map_err(|e| format!("Invalid base64 image: {e}"))?
    } else if let Some(url) = item.get("url").and_then(|v| v.as_str()) {
        client
            .get(url)
            .send()
            .await
            .map_err(|e| format!("Could not download image URL: {e}"))?
            .bytes()
            .await
            .map_err(|e| e.to_string())?
            .to_vec()
    } else {
        return Err("Image response had neither b64_json nor url.".into());
    };
    let gallery = app_data_dir(&app).join("media-gallery").join("images");
    std::fs::create_dir_all(&gallery).map_err(|e| e.to_string())?;
    let name = format!(
        "img_{}.png",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0)
    );
    let path: PathBuf = gallery.join(name);
    std::fs::write(&path, &bytes).map_err(|e| e.to_string())?;
    Ok(MediaGenerateImageResult {
        saved_path: path.to_string_lossy().into_owned(),
        provider: remote.provider.clone(),
        revised_prompt: revised,
    })
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaGenerateVideoRequest {
    pub prompt: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub duration_secs: Option<u32>,
    pub model: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaGenerateVideoResult {
    pub job_id: String,
    pub status: String,
    pub message: String,
}

#[tauri::command]
pub async fn media_generate_video(
    _app: AppHandle,
    request: MediaGenerateVideoRequest,
) -> Result<MediaGenerateVideoResult, String> {
    let prompt = request.prompt.trim();
    if prompt.is_empty() {
        return Err("Prompt is required.".into());
    }
    Ok(MediaGenerateVideoResult {
        job_id: format!("vid_{}", uuid_simple()),
        status: "queued".into(),
        message:
            "Local video generation (Wan 2.2, LTX-2.3, etc.) will run through the media engine once installed. \
             For now, download a video checkpoint from Models Hub → Video and configure the engine path in Settings. \
             Cloud video providers will be added in a later release."
                .into(),
    })
}

fn uuid_simple() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let n = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{:x}", n)
}
