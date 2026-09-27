//! Model lifecycle, Hugging Face hub, multi-provider registry and local
//! OpenAI-compatible API server commands (moved out of `main.rs`).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::json;
use tauri::{AppHandle, Emitter, State};
use tauri_plugin_dialog::DialogExt;

use crate::agent::interrupt::InterruptState;
use crate::commands::settings::{models_dir, parse_settings};
use crate::engine::{EnginePool, LoadProgressEvent, ModelInfo, ModelInitParams, TextGenerator};
use crate::remote::{ProviderConfig, ProviderKind, ProviderRole, RemoteGenerator, RemoteModelConfig};
use crate::{
    app_data_dir, install_local_model, spawn_emitter, ApiServerState, ContextState, DownloadControl,
    HubState, InferenceState, ProviderRegistryState,
};

/// Pick a GGUF model file, load it and return its metadata. Blocking model
/// load is moved to `spawn_blocking` so the webview stays responsive.
#[tauri::command]
pub async fn pick_and_load_model(
    app: AppHandle,
    state: State<'_, InferenceState>,
    context_state: State<'_, ContextState>,
    api_state: State<'_, ApiServerState>,
    params: Option<ModelInitParams>,
) -> Result<Option<ModelInfo>, String> {
    let dialog = app.dialog();
    let Some(picked) = dialog
        .file()
        .set_title("Select a GGUF model")
        .add_filter("GGUF model", &["gguf"])
        .blocking_pick_file()
    else {
        return Ok(None);
    };
    let path = picked.into_path().map_err(|e| e.to_string())?;

    let info = install_local_model(
        app,
        &state,
        &context_state,
        Some(&api_state),
        path,
        params.unwrap_or_default(),
    )
    .await?;
    Ok(Some(info))
}

/// Load a GGUF from an explicit path (used for models downloaded via the hub).
#[tauri::command]
pub async fn load_model_from_path(
    app: AppHandle,
    state: State<'_, InferenceState>,
    context_state: State<'_, ContextState>,
    api_state: State<'_, ApiServerState>,
    path: String,
    params: Option<ModelInitParams>,
) -> Result<ModelInfo, String> {
    let p = PathBuf::from(&path);
    if !p.is_file() {
        return Err(format!("Not a file: {}", p.display()));
    }
    install_local_model(
        app,
        &state,
        &context_state,
        Some(&api_state),
        p,
        params.unwrap_or_default(),
    )
    .await
}

/// Result of a successful delete, so the UI can report what was freed.
#[derive(serde::Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct DeleteModelResult {
    /// Path that was removed.
    pub path: String,
    /// Bytes reclaimed.
    pub freed_bytes: u64,
    /// True when the now-empty repo folder was removed too.
    pub removed_repo_dir: bool,
}

/// Reject anything that is not a bare filename.
///
/// `list_downloaded` only ever yields leaf names, so this never rejects a
/// legitimate target; it exists so a crafted `file_name` can never be turned
/// into a traversal.
fn is_safe_file_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.contains('/')
        && !name.contains('\\')
        && !name.contains(':')
        && !name.contains('\0')
}

/// Delete a downloaded GGUF.
///
/// Identified by `repo_id` + `file_name` rather than a caller-supplied path:
/// the target is looked up in the same `list_downloaded` scan the UI lists, so a
/// buggy or hostile frontend cannot use this to delete anything outside the
/// models directory. The loaded model is refused because it is memory-mapped
/// and would fail to unlink anyway.
#[tauri::command]
pub async fn delete_downloaded_model(
    app: AppHandle,
    state: State<'_, InferenceState>,
    repo_id: String,
    file_name: String,
) -> Result<DeleteModelResult, String> {
    let loaded = state.loaded_path.lock().await.clone();
    delete_model_file(&models_dir(&app), &repo_id, &file_name, loaded.as_deref()).await
}

/// Core of `delete_downloaded_model`, split out so the destructive logic can be
/// exercised without an `AppHandle`.
///
/// `models_dir` is the only tree that can be touched: the target is resolved
/// from a fresh `list_downloaded` scan of it, so `repo_id`/`file_name` are
/// selectors rather than path fragments. `loaded` is the currently-loaded model
/// path, which is refused because it is memory-mapped and cannot be unlinked on
/// Windows.
pub async fn delete_model_file(
    models_dir: &Path,
    repo_id: &str,
    file_name: &str,
    loaded: Option<&str>,
) -> Result<DeleteModelResult, String> {
    if !is_safe_file_name(file_name) {
        return Err("Invalid model file name.".into());
    }

    let entry = crate::hub::list_downloaded(models_dir)
        .into_iter()
        .find(|m| m.repo_id == repo_id && m.file_name == file_name)
        .ok_or_else(|| {
            format!("{file_name} is not in the models directory — nothing to delete.")
        })?;

    let target = PathBuf::from(&entry.path);
    if !target.is_file() {
        return Err(format!("Already gone: {}", target.display()));
    }

    if let Some(loaded) = loaded {
        if paths_match(Path::new(loaded), &target) {
            return Err(format!(
                "{} is the model that is currently loaded. Unload it first (the title bar has an \
                 unload button), then delete it.",
                target.display()
            ));
        }
    }

    let size = tokio::fs::metadata(&target)
        .await
        .map(|m| m.len())
        .unwrap_or(entry.size_bytes);
    tokio::fs::remove_file(&target)
        .await
        .map_err(|e| format!("Could not delete {}: {e}", target.display()))?;

    // Tidy up the per-repo folder once its last file is gone, so the hub list
    // does not keep showing empty repos. Only a direct child of models_dir.
    let mut removed_repo_dir = false;
    if let Some(parent) = target.parent() {
        // `parent` must sit *inside* `models_dir`; the repo folder is one level
        // down from it. Checking the other direction would allow removing a
        // directory that merely shares a name prefix.
        let is_repo_dir = parent != models_dir
            && parent.starts_with(models_dir)
            && std::fs::read_dir(parent)
                .map(|mut d| d.next().is_none())
                .unwrap_or(false);
        if is_repo_dir {
            removed_repo_dir = std::fs::remove_dir(parent).is_ok();
        }
    }

    Ok(DeleteModelResult {
        path: entry.path,
        freed_bytes: size,
        removed_repo_dir,
    })
}

/// Compare two paths tolerantly: fall back to a plain string compare when
/// canonicalization fails (missing file, permissions).
fn paths_match(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(x), Ok(y)) => x == y,
        _ => a == b,
    }
}

/// Fetch the available model ids for a remote provider so the connection UI
/// can offer them as a dropdown (see `remote::list_models`).
#[tauri::command]
pub async fn list_remote_models(config: RemoteModelConfig) -> Result<Vec<String>, String> {
    crate::remote::list_models(&config).await
}

/// Configure and activate the remote (OpenAI-compatible) backend. Swaps out
/// any local pool, aligns the context budget, and fires the same
/// `model-loaded` flow as a local load so the UI stays backend-agnostic.
#[tauri::command]
pub async fn configure_remote_model(
    app: AppHandle,
    state: State<'_, InferenceState>,
    interrupt_state: State<'_, InterruptState>,
    context_state: State<'_, ContextState>,
    api_state: State<'_, ApiServerState>,
    providers: State<'_, ProviderRegistryState>,
    config: RemoteModelConfig,
) -> Result<ModelInfo, String> {
    interrupt_state.trigger();
    // Remote backends are cheap to duplicate: give each worker its own client.
    let workers = 4usize;
    let event_tx = state
        .worker_tx
        .lock()
        .await
        .clone()
        .unwrap_or_else(|| spawn_emitter(app.clone()));
    // Let the registry overrule the Editor backend when a provider is routed
    // for it; with an empty registry (default) this resolves to the caller's
    // config, preserving today's single-remote behaviour exactly.
    let effective_config = {
        let reg = providers.0.lock().await;
        match reg.route(ProviderRole::Editor) {
            Some(p) if p.kind != ProviderKind::Local => {
                p.to_remote_model_config().unwrap_or_else(|_| config.clone())
            }
            _ => config.clone(),
        }
    };
    let factory = || {
        RemoteGenerator::new(effective_config.clone())
            .map(|g| Box::new(g) as Box<dyn TextGenerator>)
    };
    let pool = EnginePool::spawn_with(factory, event_tx, workers)?;
    let info = pool.info();

    let mut pool_guard = state.pool.lock().await;
    let mut info_guard = state.info.lock().await;
    *pool_guard = Some(Arc::new(pool));
    *info_guard = Some(info.clone());
    drop(pool_guard);
    drop(info_guard);
    api_state.engine.set(state.pool.lock().await.clone());
    context_state
        .inner
        .lock()
        .await
        .set_limit(info.context_size as usize);

    let _ = app.emit_to(
        "main",
        "model-load-progress",
        LoadProgressEvent {
            stage: "done",
            progress: 1.0,
        },
    );
    let _ = app.emit("model-loaded", &info);
    Ok(info)
}

// ---------------------------------------------------------------------------
// Multi-provider registry commands (drive `ProviderRegistry` from the UI).
// These persist providers and the role → provider map in Tauri managed state;
// `configure_remote_model` already consults `route(Editor)` at load time.
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn providers_upsert(
    state: State<'_, ProviderRegistryState>,
    provider: ProviderConfig,
) -> Result<String, String> {
    state.0.lock().await.add_provider(provider.clone());
    Ok(provider.id)
}

#[tauri::command]
pub async fn providers_remove(
    state: State<'_, ProviderRegistryState>,
    id: String,
) -> Result<bool, String> {
    Ok(state.0.lock().await.remove_provider(&id))
}

#[tauri::command]
pub async fn providers_set_role(
    state: State<'_, ProviderRegistryState>,
    role: ProviderRole,
    provider_id: String,
) -> Result<(), String> {
    state.0.lock().await.set_role_provider(role, provider_id);
    Ok(())
}

#[tauri::command]
pub async fn providers_clear_role(
    state: State<'_, ProviderRegistryState>,
    role: ProviderRole,
) -> Result<(), String> {
    state.0.lock().await.clear_role(role);
    Ok(())
}

#[tauri::command]
pub async fn providers_route(
    state: State<'_, ProviderRegistryState>,
    role: ProviderRole,
) -> Result<Option<ProviderConfig>, String> {
    Ok(state.0.lock().await.route(role).cloned())
}

#[tauri::command]
pub async fn providers_list(
    state: State<'_, ProviderRegistryState>,
) -> Result<Vec<ProviderConfig>, String> {
    Ok(state.0.lock().await.providers().to_vec())
}

#[tauri::command]
pub async fn unload_model(
    state: State<'_, InferenceState>,
    interrupt_state: State<'_, InterruptState>,
    api_state: State<'_, ApiServerState>,
) -> Result<(), String> {
    interrupt_state.trigger();
    crate::logging::info(None, "model", "unloading model …");
    let mut pool_guard = state.pool.lock().await;
    let mut info_guard = state.info.lock().await;
    *pool_guard = None;
    *info_guard = None;
    drop(info_guard);
    drop(pool_guard);
    api_state.engine.set(None);
    *state.loaded_path.lock().await = None;
    crate::logging::info(None, "model", "model unloaded");
    Ok(())
}

/// Poll the current model metadata (used on webview startup / reconnect).
#[tauri::command]
pub async fn model_status(state: State<'_, InferenceState>) -> Result<Option<ModelInfo>, String> {
    let info_guard = state.info.lock().await;
    Ok(info_guard.clone())
}

/// Absolute GGUF path currently loaded, for display next to Load/Unload.
#[tauri::command]
pub async fn loaded_model_path(state: State<'_, InferenceState>) -> Result<Option<String>, String> {
    Ok(state.loaded_path.lock().await.clone())
}

// ---------------------------------------------------------------------------
// Hugging Face hub
// ---------------------------------------------------------------------------

/// Search the hub for GGUF repos (public models, no auth).
#[tauri::command]
pub async fn hf_search(query: String, limit: Option<usize>) -> Result<Vec<crate::hub::HfModel>, String> {
    crate::hub::search(&query, limit.unwrap_or(20)).await
}

/// Start streaming a GGUF from the hub into `{app_data}/models/…`. Progress
/// arrives on the `hf-download-progress` event channel; one download per
/// repo+file at a time. The download is *resumable*: pausing or cancelling
/// keeps the `*.part` file so `hf_resume_download` / a later call continues
/// from the same byte offset.
#[tauri::command]
pub async fn hf_download_model(
    app: AppHandle,
    hub_state: State<'_, HubState>,
    repo_id: String,
    file_name: String,
) -> Result<(), String> {
    let key = format!("{repo_id}::{file_name}");
    {
        let mut active = hub_state.active.lock().await;
        if let Some(control) = active.get(&key) {
            if !control.cancel.is_cancelled() {
                // Already running (or paused awaiting resume) – no-op.
                return Ok(());
            }
        }
        active.insert(key.clone(), DownloadControl::new());
    }
    let control = hub_state.active.lock().await.get(&key).cloned().unwrap();
    let registry = hub_state.active.clone();

    let models_dir = models_dir(&app);
    let app_sink = app.clone();
    let repo = repo_id.clone();
    let file = file_name.clone();
    tokio::spawn(async move {
        loop {
            if control.cancel.is_cancelled() {
                let _ = app_sink.emit(
                    "hf-download-progress",
                    json!({ "repoId": repo, "file": file, "cancelled": true }),
                );
                registry.lock().await.remove(&key);
                return;
            }
            while control.paused.load(std::sync::atomic::Ordering::Relaxed) {
                tokio::select! {
                    _ = control.resume.notified() => {}
                    _ = control.cancel.clone().cancelled_owned() => {
                        let _ = app_sink.emit(
                            "hf-download-progress",
                            json!({ "repoId": repo, "file": file, "cancelled": true }),
                        );
                        registry.lock().await.remove(&key);
                        return;
                    }
                }
            }
            let result = crate::hub::download_file(&models_dir, &repo, &file, &control.cancel, &control.paused, |p| {
                let _ = app_sink.emit(
                    "hf-download-progress",
                    json!({
                        "repoId": repo,
                        "file": file,
                        "receivedBytes": p.received,
                        "totalBytes": p.total,
                        "done": p.done,
                    }),
                );
            })
            .await;
            match result {
                Ok(_) => {
                    registry.lock().await.remove(&key);
                    let _ = app_sink.emit(
                        "hf-download-progress",
                        json!({ "repoId": repo, "file": file, "done": true }),
                    );
                    // Best-effort companion fetch: pull the repo's `tokenizer.json`
                    // alongside the weights so exact token counting engages on load.
                    if crate::hub::download_tokenizer(&models_dir, &repo).await {
                        crate::logging::info(
                            None,
                            "hub.tokenizer",
                            &format!("fetched tokenizer for `{repo}`"),
                        );
                    }
                    return;
                }
                Err(e) if e == "__paused__" => {
                    let _ = app_sink.emit(
                        "hf-download-progress",
                        json!({ "repoId": repo, "file": file, "paused": true }),
                    );
                    // Loop back; the top of the loop blocks until resumed.
                }
                Err(e) if e == "__cancelled__" => {
                    let _ = app_sink.emit(
                        "hf-download-progress",
                        json!({ "repoId": repo, "file": file, "cancelled": true }),
                    );
                    registry.lock().await.remove(&key);
                    return;
                }
                Err(e) => {
                    let _ = app_sink.emit(
                        "hf-download-progress",
                        json!({ "repoId": repo, "file": file, "error": e }),
                    );
                    registry.lock().await.remove(&key);
                    return;
                }
            }
        }
    });
    Ok(())
}

/// Pause an in-progress download (keeps the partial file for later resume).
#[tauri::command]
pub async fn hf_pause_download(
    hub_state: State<'_, HubState>,
    repo_id: String,
    file_name: String,
) -> Result<(), String> {
    let key = format!("{repo_id}::{file_name}");
    if let Some(control) = hub_state.active.lock().await.get(&key) {
        control
            .paused
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }
    Ok(())
}

/// Currently running or paused downloads so the UI can restore state on
/// remount (the webview is reloaded while a download is in progress).
#[derive(serde::Serialize)]
pub(crate) struct ActiveDownload {
    repo_id: String,
    file_name: String,
    paused: bool,
}

#[tauri::command]
pub async fn hf_list_active(
    hub_state: State<'_, HubState>,
) -> Result<Vec<ActiveDownload>, String> {
    use std::sync::atomic::Ordering;
    let active = hub_state.active.lock().await;
    Ok(active
        .iter()
        .map(|(k, c)| {
            let (repo_id, file_name) = k.split_once("::").unwrap_or((k, ""));
            ActiveDownload {
                repo_id: repo_id.to_string(),
                file_name: file_name.to_string(),
                paused: c.paused.load(Ordering::Relaxed),
            }
        })
        .collect())
}

/// Resume a paused (or previously cancelled / partially downloaded) file.
#[tauri::command]
pub async fn hf_resume_download(
    app: AppHandle,
    hub_state: State<'_, HubState>,
    repo_id: String,
    file_name: String,
) -> Result<(), String> {
    let key = format!("{repo_id}::{file_name}");
    let mut active = hub_state.active.lock().await;
    let control = match active.get(&key) {
        Some(control) if !control.cancel.is_cancelled() => control.clone(),
        Some(_control) => {
            // Cancelled earlier: start a fresh control.
            active.remove(&key).unwrap();
            drop(active);
            return hf_download_model(app, hub_state, repo_id, file_name).await;
        }
        None => {
            // No control at all: start (this resumes any leftover `.part`).
            drop(active);
            return hf_download_model(app, hub_state, repo_id, file_name).await;
        }
    };
    control.paused.store(false, std::sync::atomic::Ordering::Relaxed);
    control.resume.notify_waiters();
    Ok(())
}

/// Cancel an in-progress hub download.
///
/// The `*.part` file is kept by default so a later resume continues where it
/// left off. Pass `delete_part: true` ("Cancel & delete file") to remove the
/// partial file too – the file is only deleted *after* the download task has
/// exited (it may still hold the handle open for a moment).
#[tauri::command]
pub async fn hf_cancel_download(
    app: AppHandle,
    hub_state: State<'_, HubState>,
    repo_id: String,
    file_name: String,
    delete_part: Option<bool>,
) -> Result<(), String> {
    let key = format!("{repo_id}::{file_name}");
    if let Some(control) = hub_state.active.lock().await.get(&key) {
        control.cancel.cancel();
    }
    if delete_part.unwrap_or(false) {
        let registry = hub_state.active.clone();
        let models_dir = models_dir(&app);
        let part = crate::hub::dest_path(&models_dir, &repo_id, &file_name).with_extension("part");
        tokio::spawn(async move {
            // Wait until the download task removes itself from `active` (it can
            // still be holding the `.part` handle), then delete the file.
            for _ in 0..100 {
                if !registry.lock().await.contains_key(&key) {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
            let _ = std::fs::remove_file(&part);
        });
    }
    Ok(())
}

/// GGUF files already downloaded through the hub.
#[tauri::command]
pub async fn list_downloaded_models(
    app: AppHandle,
) -> Result<Vec<crate::hub::DownloadedModel>, String> {
    Ok(crate::hub::list_downloaded(&models_dir(&app)))
}

/// Startup convenience: load a model without user interaction.
///
/// Resolution order:
///   1. `modelPath` persisted in `settings.json` (last successful load),
///   2. any `*.gguf` previously downloaded through the HF browser,
///   3. any `*.gguf` directly inside a `models/` folder next to the working
///      directory (the dev-checkout layout, e.g. `D:\ai\models`).
///
/// Returns `Ok(None)` when nothing suitable exists — that is not an error,
/// just a first-run state where the user still picks a model manually.
#[tauri::command]
pub async fn auto_load_model(
    app: AppHandle,
    state: State<'_, InferenceState>,
    context_state: State<'_, ContextState>,
    api_state: State<'_, ApiServerState>,
) -> Result<Option<ModelInfo>, String> {
    if state.pool.lock().await.is_some() {
        return Ok(state.info.lock().await.clone());
    }

    let mut candidates: Vec<PathBuf> = Vec::new();
    let saved_model_path: Option<String> = {
        let file = app_data_dir(&app).join("settings.json");
        match tokio::fs::read_to_string(&file).await {
            Ok(text) => parse_settings(&text).model_path.clone(),
            Err(_) => None,
        }
    };
    if let Some(p) = saved_model_path {
        candidates.push(PathBuf::from(p));
    }
    for m in crate::hub::list_downloaded(&models_dir(&app)) {
        candidates.push(PathBuf::from(m.path));
    }
    if let Ok(mut entries) = tokio::fs::read_dir(Path::new("models")).await {
        while let Ok(Some(entry)) = entries.next_entry().await {
            let p = entry.path();
            let name = p
                .file_name()
                .map(|n| n.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            if p.extension().and_then(|e| e.to_str()) == Some("gguf") && !name.contains("imatrix") {
                candidates.push(p);
            }
        }
    }

    for path in candidates {
        if !path.is_file() {
            continue;
        }
        crate::logging::info(None, "model", &format!("auto-loading {}", path.display()));
        return install_local_model(
            app,
            &state,
            &context_state,
            Some(&api_state),
            path,
            // Empty explicit params: `install_local_model` resolves the
            // persisted per-path config (falling back to `"default"` and the
            // legacy `params`/`nWorkers` settings), so restores honour the
            // per-model configuration.
            Default::default(),
        )
        .await
        .map(Some);
    }
    crate::logging::warn(None, "model", "no local GGUF found to auto-load");
    Ok(None)
}

// ---------------------------------------------------------------------------
// Local OpenAI-compatible API server
// ---------------------------------------------------------------------------

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ApiServerStatus {
    running: bool,
    port: Option<u16>,
}

/// Start the loopback-only OpenAI-compatible server (idempotent).
#[tauri::command]
pub async fn api_server_start(
    state: State<'_, ApiServerState>,
    port: Option<u16>,
) -> Result<ApiServerStatus, String> {
    let mut guard = state.server.lock().await;
    if let Some(handle) = guard.as_ref() {
        return Ok(ApiServerStatus {
            running: true,
            port: Some(handle.port),
        });
    }
    let handle = crate::api_server::start(state.engine.clone(), port.unwrap_or(8080))?;
    let port = handle.port;
    *guard = Some(handle);
    Ok(ApiServerStatus {
        running: true,
        port: Some(port),
    })
}

/// Stop the server (no-op when not running).
#[tauri::command]
pub async fn api_server_stop(state: State<'_, ApiServerState>) -> Result<ApiServerStatus, String> {
    let mut guard = state.server.lock().await;
    *guard = None; // dropping the handle closes the listener
    Ok(ApiServerStatus {
        running: false,
        port: None,
    })
}

/// Current server status.
#[tauri::command]
pub async fn api_server_status(state: State<'_, ApiServerState>) -> Result<ApiServerStatus, String> {
    let guard = state.server.lock().await;
    Ok(match guard.as_ref() {
        Some(h) => ApiServerStatus {
            running: true,
            port: Some(h.port),
        },
        None => ApiServerStatus {
            running: false,
            port: None,
        },
    })
}

#[cfg(test)]
mod delete_tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn safe_file_names_are_bare_leaves() {
        for good in ["model.gguf", "qwen2.5-coder-7b-instruct-q8_0-00001-of-00003.gguf"] {
            assert!(is_safe_file_name(good), "should accept {good}");
        }
    }

    #[test]
    fn unsafe_file_names_are_rejected() {
        for bad in [
            "",
            ".",
            "..",
            "../../Windows/System32/drivers/etc/hosts",
            "..\\..\\secret.gguf",
            "sub/dir/model.gguf",
            "sub\\dir\\model.gguf",
            "C:model.gguf",
            "model.gguf\0.txt",
        ] {
            assert!(!is_safe_file_name(bad), "should reject {bad:?}");
        }
    }

    #[test]
    fn paths_match_tolerates_case_differences() {
        // Windows filesystems are case-insensitive, so the loaded-path guard
        // must not be bypassed by a differently-cased copy of the same path.
        let a = PathBuf::from(r"D:\ai\models\qwen2.5-0.5b-instruct-q4_k_m.gguf");
        let b = PathBuf::from(r"D:\AI\Models\Qwen2.5-0.5B-Instruct-Q4_K_M.gguf");
        assert!(paths_match(&a, &b));
    }

    #[test]
    fn paths_match_distinguishes_different_files() {
        let a = Path::new(r"D:\ai\models\a.gguf");
        let b = Path::new(r"D:\ai\models\b.gguf");
        assert!(!paths_match(a, b));
    }

    // A scratch models dir shaped like the real one, for exercising deletion
    // against the filesystem rather than just the guard clauses.
    struct Scratch {
        root: PathBuf,
    }

    impl Scratch {
        fn new(tag: &str) -> Self {
            let root = std::env::temp_dir().join(format!("ai-editor-del-test-{tag}"));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(&root).expect("create scratch dir");
            Scratch { root }
        }

        /// Write a fake repo folder containing a runnable-named GGUF.
        fn repo(&self, repo: &str, file: &str, bytes: usize) -> PathBuf {
            let dir = self.root.join(repo);
            std::fs::create_dir_all(&dir).expect("create repo dir");
            let path = dir.join(file);
            std::fs::write(&path, vec![0u8; bytes]).expect("write fake gguf");
            path
        }

        fn loose(&self, file: &str, bytes: usize) -> PathBuf {
            let path = self.root.join(file);
            std::fs::write(&path, vec![0u8; bytes]).expect("write fake gguf");
            path
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    #[tokio::test]
    async fn deletes_a_repo_file_and_cleans_up_the_folder() {
        let s = Scratch::new("repo");
        let path = s.repo("author--Repo-GGUF", "model.gguf", 2048);

        let res = delete_model_file(&s.root, "author/Repo-GGUF", "model.gguf", None)
            .await
            .expect("delete should succeed");

        assert!(!path.exists(), "file should be gone");
        assert_eq!(res.freed_bytes, 2048);
        assert!(res.removed_repo_dir, "emptied repo folder should be removed");
        assert!(
            !s.root.join("author--Repo-GGUF").exists(),
            "the emptied repo folder should be gone"
        );
        assert!(
            s.root.exists(),
            "the models dir itself must survive - only the repo subfolder is tidied"
        );
    }

    #[tokio::test]
    async fn keeps_the_folder_when_a_sibling_shard_remains() {
        let s = Scratch::new("sibling");
        s.repo(
            "author--Repo-GGUF",
            "model-00001-of-00002.gguf",
            512,
        );
        s.repo("author--Repo-GGUF", "model-00002-of-00002.gguf", 512);

        let res = delete_model_file(
            &s.root,
            "author/Repo-GGUF",
            "model-00001-of-00002.gguf",
            None,
        )
        .await
        .expect("delete should succeed");

        assert!(!res.removed_repo_dir, "folder still holds a shard");
        assert!(s
            .root
            .join("author--Repo-GGUF")
            .join("model-00002-of-00002.gguf")
            .exists());
    }

    #[tokio::test]
    async fn does_not_remove_a_folder_outside_the_models_dir() {
        // models_dir = <tmp>/...-inner; the repo folder is a *sibling* whose name
        // shares the prefix. Removing it would reach outside the models dir.
        let base = std::env::temp_dir().join("ai-editor-del-prefix");
        let _ = std::fs::remove_dir_all(&base);
        let inner = base.join("inner");
        let sibling = base.join("inner-sibling");
        std::fs::create_dir_all(&inner).expect("create inner");
        std::fs::create_dir_all(&sibling).expect("create sibling");
        let file = sibling.join("model.gguf");
        std::fs::write(&file, vec![0u8; 16]).expect("write gguf");

        // Deleting through `inner` must not find (or touch) the sibling's file.
        let res = delete_model_file(&inner, "local", "model.gguf", None).await;
        assert!(res.is_err(), "sibling file is not listable from inner");
        assert!(file.exists(), "file outside the models dir must survive");

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn deletes_a_loose_file_in_the_models_dir() {
        let s = Scratch::new("loose");
        let path = s.loose("qwen2.5-0.5b-instruct-q4_k_m.gguf", 1024);

        delete_model_file(&s.root, "local", "qwen2.5-0.5b-instruct-q4_k_m.gguf", None)
            .await
            .expect("delete should succeed");

        assert!(!path.exists());
    }

    #[tokio::test]
    async fn refuses_to_delete_the_loaded_model() {
        let s = Scratch::new("loaded");
        let path = s.repo("author--Repo-GGUF", "model.gguf", 1024);

        let err = delete_model_file(
            &s.root,
            "author/Repo-GGUF",
            "model.gguf",
            Some(path.to_str().unwrap()),
        )
        .await
        .expect_err("the loaded model must be protected");

        assert!(err.contains("currently loaded"), "unhelpful error: {err}");
        assert!(path.exists(), "file must survive the refusal");
    }

    #[tokio::test]
    async fn refuses_traversal_out_of_the_models_dir() {
        let s = Scratch::new("traversal");
        let outside = s.root.parent().expect("parent of scratch").join("ai-editor-del-outside.gguf");
        std::fs::write(&outside, b"precious").expect("write decoy");

        for bad in [
            "../../ai-editor-del-outside.gguf",
            "..\\..\\ai-editor-del-outside.gguf",
            "/etc/passwd",
        ] {
            let err = delete_model_file(&s.root, "local", bad, None)
                .await
                .expect_err("must reject {bad}");
            assert!(err.contains("Invalid model file name"), "got: {err}");
        }
        assert!(outside.exists(), "decoy outside the models dir must survive");
        let _ = std::fs::remove_file(&outside);
    }

    #[tokio::test]
    async fn reports_when_the_file_is_not_listed() {
        let s = Scratch::new("missing");
        let err = delete_model_file(&s.root, "local", "not-here.gguf", None)
            .await
            .expect_err("nothing to delete");
        assert!(err.contains("nothing to delete"), "got: {err}");
    }

    #[tokio::test]
    async fn ignores_non_gguf_files() {
        let s = Scratch::new("nongguf");
        // A README dropped next to the models: not a runnable model, so it must
        // not be deletable through the model-delete path.
        let dir = s.root.join("author--Repo-GGUF");
        std::fs::create_dir_all(&dir).expect("create dir");
        let readme = dir.join("README.md");
        std::fs::write(&readme, b"hi").expect("write readme");

        let err = delete_model_file(&s.root, "author/Repo-GGUF", "README.md", None)
            .await
            .expect_err("only GGUFs are deletable");
        assert!(readme.exists());
        let _ = err;
    }
}
