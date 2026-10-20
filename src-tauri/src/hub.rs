//! Hugging Face model hub integration.
//!
//! * `search` queries the public `huggingface.co/api/models` endpoint for GGUF
//!   repositories (no auth needed for public models) and extracts the
//!   downloadable `*.gguf` siblings with their sizes.
//! * `download_file` streams one GGUF into `{models_dir}` with progress
//!   callbacks so the UI can render a progress bar; cooperative cancellation
//!   via `tokio_util::sync::CancellationToken`.
//!
//! Everything here talks to `huggingface.co` directly with plain HTTPS —
//! no extra SDK dependency.

use std::path::{Path, PathBuf};

use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

const HF_API_BASE: &str = "https://huggingface.co";
/// Hard cap per model file (200 GiB) purely as a sanity guard.
const MAX_FILE_BYTES: u64 = 200 * 1024 * 1024 * 1024;
/// How often the download loop pauses to re-read the cancel/pause flags.
/// This bounds how late a user-issued cancel/pause can land.
const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);

fn http_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .user_agent("ai-editor/0.1 (+local)")
        .connect_timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| format!("HTTP client error: {e}"))
}

/// One downloadable GGUF weight file inside a repository.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HfFile {
    /// File name within the repo, e.g. `model-q4_k_m.gguf`.
    pub name: String,
    pub size: Option<u64>,
}

/// A GGUF repository matching a hub search.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HfModel {
    /// Full repo id, e.g. `bartowski/Meta-Llama-3.1-8B-Instruct-GGUF`.
    pub repo_id: String,
    pub author: Option<String>,
    pub likes: i64,
    pub downloads: i64,
    /// `.gguf` siblings with sizes (when known).
    pub files: Vec<HfFile>,
}

#[derive(Deserialize)]
struct ApiModel {
    id: String,
    #[serde(default)]
    author: Option<String>,
    #[serde(default)]
    likes: i64,
    #[serde(default)]
    downloads: i64,
    #[serde(default)]
    siblings: Vec<ApiSibling>,
}

#[derive(Deserialize)]
struct ApiSibling {
    rfilename: String,
    #[serde(default)]
    size: Option<u64>,
}

#[derive(Deserialize)]
struct ApiTreeEntry {
    path: String,
    #[serde(default)]
    size: Option<u64>,
    #[serde(default)]
    lfs: Option<ApiTreeLfs>,
}

#[derive(Deserialize)]
struct ApiTreeLfs {
    #[serde(default)]
    size: Option<u64>,
}

/// Per-repo file sizes (`/api/models/{repo_id}/tree/main`). The flat search
/// API frequently reports `null` sizes for GGUF siblings, so each repo that is
/// missing sizes is back-filled here with a single cheap request.
async fn fetch_tree_sizes(
    client: &reqwest::Client,
    repo_id: &str,
) -> std::collections::HashMap<String, u64> {
    let url = format!("{HF_API_BASE}/api/models/{}/tree/main", repo_id);
    let Ok(resp) = client.get(&url).send().await else {
        return std::collections::HashMap::new();
    };
    if !resp.status().is_success() {
        return std::collections::HashMap::new();
    }
    let Ok(entries) = resp.json::<Vec<ApiTreeEntry>>().await else {
        return std::collections::HashMap::new();
    };
    entries
        .into_iter()
        .filter_map(|e| {
            let size = e.size.or_else(|| e.lfs.and_then(|l| l.size))?;
            Some((e.path, size))
        })
        .collect()
}

/// Search the hub for GGUF model repos. `full` + `blobs` query params make
/// the API include sibling filenames and their byte sizes.
pub async fn search(query: &str, limit: usize) -> Result<Vec<HfModel>, String> {
    let limit = limit.clamp(1, 50);
    let url = format!(
        "{HF_API_BASE}/api/models?search={}&filter=gguf&sort=downloads&direction=-1&limit={limit}&full=true&blobs=true",
        urlencode(query)
    );
    let client = http_client()?;
    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("HF search failed: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("HF search returned HTTP {}", resp.status()));
    }
    let models: Vec<ApiModel> = resp
        .json()
        .await
        .map_err(|e| format!("Bad HF response: {e}"))?;

    let mut out: Vec<HfModel> = models
        .into_iter()
        .map(|m| HfModel {
            files: m
                .siblings
                .into_iter()
                .filter(|s| s.rfilename.to_ascii_lowercase().ends_with(".gguf"))
                .map(|s| HfFile {
                    name: s.rfilename,
                    size: s.size,
                })
                .collect(),
            repo_id: m.id,
            author: m.author,
            likes: m.likes,
            downloads: m.downloads,
        })
        .filter(|m| !m.files.is_empty())
        .collect();

    // Back-fill missing sizes for repos the search API gave us nothing for.
    let missing: Vec<String> = out
        .iter()
        .filter(|m| m.files.iter().any(|f| f.size.is_none()))
        .map(|m| m.repo_id.clone())
        .collect();
    if !missing.is_empty() {
        use futures_util::stream;
        let sizes: Vec<(String, std::collections::HashMap<String, u64>)> = stream::iter(missing)
            .map(|repo| {
                let client = client.clone();
                async move {
                    let repo2 = repo.clone();
                    let map = tokio::time::timeout(
                        std::time::Duration::from_secs(10),
                        fetch_tree_sizes(&client, &repo),
                    )
                    .await
                    .unwrap_or_else(|_| std::collections::HashMap::new());
                    (repo2, map)
                }
            })
            .buffer_unordered(8)
            .collect()
            .await;

        let mut index: std::collections::HashMap<String, std::collections::HashMap<String, u64>> =
            sizes.into_iter().collect();
        for model in out.iter_mut() {
            if let Some(size_map) = index.remove(&model.repo_id) {
                for f in model.files.iter_mut() {
                    if f.size.is_none() {
                        f.size = size_map.get(&f.name).copied();
                    }
                }
            }
        }
    }

    Ok(out)
}

/// Destination path for a repo file under `models_dir`
/// (`{models_dir}/{author}--{repo}/{file_name}`).
pub fn dest_path(models_dir: &Path, repo_id: &str, file_name: &str) -> PathBuf {
    // Flatten `org/name` into one folder level (Windows-safe separators).
    let slug = repo_id.replace(['/', '\\'], "--");
    models_dir
        .join(slug)
        .join(file_name.replace(['/', '\\'], "_"))
}

/// Destination folder for a repo under `models_dir`
/// (`{models_dir}/{author}--{repo}/`), mirroring [`dest_path`]'s flat slug.
pub fn repo_dir(models_dir: &Path, repo_id: &str) -> PathBuf {
    let slug = repo_id.replace(['/', '\\'], "--");
    models_dir.join(slug)
}

/// Best-effort fetch of a repo's `tokenizer.json` into its folder so the
/// orchestrator can register exact token counts for a downloaded model.
/// Tolerant: GGUF repos that do not ship a tokenizer (they inherit the base
/// model's tokenizer) simply 404 and this returns `false` rather than failing
/// the model download. Returns `true` when the tokenizer is on disk.
pub async fn download_tokenizer(models_dir: &Path, repo_id: &str) -> bool {
    let dest = repo_dir(models_dir, repo_id).join("tokenizer.json");
    if dest.is_file() {
        return true;
    }
    let url = format!("{HF_API_BASE}/{repo_id}/resolve/main/tokenizer.json");
    let client = match http_client() {
        Ok(c) => c,
        Err(_) => return false,
    };
    let resp = match client
        .get(&url)
        .timeout(std::time::Duration::from_secs(60))
        .send()
        .await
    {
        Ok(r) => r,
        Err(_) => return false,
    };
    if !resp.status().is_success() {
        return false;
    }
    let bytes = match resp.bytes().await {
        Ok(b) if !b.is_empty() => b,
        _ => return false,
    };
    // tokenizer.json is a few MB at most; guard against a misrouted URL.
    if bytes.len() > 50 * 1024 * 1024 {
        return false;
    }
    let _ = std::fs::create_dir_all(repo_dir(models_dir, repo_id));
    match tokio::fs::write(&dest, &bytes).await {
        Ok(_) => true,
        Err(_) => false,
    }
}

/// Progress callback payload handed to `download_file`.
pub struct DownloadProgress {
    pub received: u64,
    pub total: Option<u64>,
    /// True once the file is fully written and flushed.
    pub done: bool,
}

/// Stream `https://huggingface.co/{repo}/resolve/main/{file}` into
/// `dest_path(models_dir, repo, file)`.
///
/// * **Resume** — a partial `*.part` file left by a previous pause/cancel is
///   detected and the download is continued with a `Range: bytes=N-` request
///   (appending to the file), so pausing/restarting does not re-download.
/// * **Pause** — when `paused` is set the loop stops streaming and returns
///   `Err("__paused__")` *keeping* the partial file so a later call resumes.
/// * **Cancel** — `Err("__cancelled__")` is returned, again keeping the
///   partial file (so the UI can restart where it left off).
///
/// `on_progress` fires after every chunk (~256 KiB granularity).
pub async fn download_file(
    models_dir: &Path,
    repo_id: &str,
    file_name: &str,
    cancel: &CancellationToken,
    paused: &std::sync::atomic::AtomicBool,
    mut on_progress: impl FnMut(DownloadProgress),
) -> Result<PathBuf, String> {
    use std::sync::atomic::Ordering;
    use tokio::io::AsyncWriteExt;
    use tokio::time::{Duration, Instant};

    const STALL_SECS: u64 = 20;
    /// Consecutive stalls tolerated before surfacing a hard error. Each stall
    /// auto-resumes from the received byte offset, so HF CDN hiccups that used
    /// to hang a download forever now heal themselves.
    const MAX_STALLS: u32 = 3;

    let url = format!("{HF_API_BASE}/{repo_id}/resolve/main/{file_name}");
    let dest = dest_path(models_dir, repo_id, file_name);
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir failed: {e}"))?;
    }

    let tmp = dest.with_extension("part");
    let existing: u64 = tmp
        .metadata()
        .map(|m| m.len())
        .ok()
        .filter(|_| tmp.is_file())
        .unwrap_or(0);

    let mut received: u64 = existing;
    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&tmp)
        .await
        .map_err(|e| format!("Failed to open `{}`: {e}", tmp.display()))?;
    let mut stalls: u32 = 0;

    // Outer connection loop: reconnects (auto-resuming via `Range`) when the
    // inner stream goes silent, instead of leaving the download frozen at 0%
    // forever (HF CDN hiccups used to hang rides until the process died).
    'outer: loop {
        if cancel.is_cancelled() {
            drop(file);
            return Err("__cancelled__".to_string());
        }
        if paused.load(Ordering::Relaxed) {
            drop(file);
            return Err("__paused__".to_string());
        }

        // Connect with a Range resume header; bounded so neither a dead
        // server nor a cancel can block the reconnect path indefinitely.
        let mut req = http_client()?.get(&url).timeout(Duration::MAX);
        if received > 0 {
            req = req.header("Range", format!("bytes={received}-"));
        }
        let send = req.send();
        let resp = tokio::select! {
            r = tokio::time::timeout(Duration::from_secs(STALL_SECS), send) => match r {
                Ok(Ok(resp)) => resp,
                Ok(Err(e)) => {
                    let _ = e;
                    drop(file);
                    return Err(format!("Download failed: {e}"));
                }
                Err(_) => {
                    stalls += 1;
                    if stalls > MAX_STALLS {
                        drop(file);
                        return Err(format!(
                            "Download stalled (no server response for {STALL_SECS}s, {MAX_STALLS} retries) — kept a partial file; retry to resume"
                        ));
                    }
                    continue 'outer;
                }
            },
            _ = cancel.clone().cancelled_owned() => {
                drop(file);
                return Err("__cancelled__".to_string());
            }
        };
        if !resp.status().is_success() {
            let code = resp.status();
            drop(file);
            return Err(format!("Download returned HTTP {code}"));
        }

        // Negotiate resume: `206 Partial Content` means the server honoured
        // our `Range`; a plain `200` means it ignored it, so start over.
        let resumed = resp.status() == reqwest::StatusCode::PARTIAL_CONTENT;
        if !resumed && received > 0 {
            drop(file);
            let _ = tokio::fs::remove_file(&tmp).await;
            file = tokio::fs::File::create(&tmp)
                .await
                .map_err(|e| format!("Failed to create `{}`: {e}", tmp.display()))?;
            received = 0;
            stalls = 0;
        }
        let total = resp.content_length().map(|len| len + received);
        let mut stream = resp.bytes_stream();

        let mut last_data = Instant::now();
        loop {
            if cancel.is_cancelled() {
                drop(file);
                return Err("__cancelled__".to_string());
            }
            if paused.load(Ordering::Relaxed) {
                drop(file);
                return Err("__paused__".to_string());
            }
            // Poll with a 1s ceiling between chunk reads so cancel/pause land
            // promptly; a silent upstream is detected via `last_data`.
            match tokio::time::timeout(POLL_INTERVAL, stream.next()).await {
                Ok(Some(chunk)) => {
                    let chunk = chunk.map_err(|e| format!("Connection lost mid-download: {e}"))?;
                    received += chunk.len() as u64;
                    if received > MAX_FILE_BYTES {
                        drop(file);
                        let _ = tokio::fs::remove_file(&tmp).await;
                        return Err("File exceeds sanity size cap".to_string());
                    }
                    file.write_all(&chunk)
                        .await
                        .map_err(|e| format!("Write failed: {e}"))?;
                    last_data = Instant::now();
                    stalls = 0;
                    on_progress(DownloadProgress {
                        received,
                        total,
                        done: false,
                    });
                }
                Ok(None) => {
                    // EOF – stream finished normally.
                    file.flush()
                        .await
                        .map_err(|e| format!("Flush failed: {e}"))?;
                    drop(file);
                    tokio::fs::rename(&tmp, &dest)
                        .await
                        .map_err(|e| format!("Finalize failed: {e}"))?;
                    on_progress(DownloadProgress {
                        received,
                        total,
                        done: true,
                    });
                    return Ok(dest);
                }
                Err(_elapsed) => {
                    if last_data.elapsed() >= Duration::from_secs(STALL_SECS) {
                        stalls += 1;
                        if stalls > MAX_STALLS {
                            drop(file);
                            return Err(format!(
                                "Download stalled (no data for {STALL_SECS}s, {MAX_STALLS} retries) — kept a partial file; retry to resume"
                            ));
                        }
                        // Reconnect from the current byte offset and keep
                        // appending to the same `.part` file.
                        break;
                    }
                }
            }
        }
        // Fall through to the outer loop for a reconnect.
        continue 'outer;
    }
}

/// A file that can actually be loaded as a model: a `*.gguf` that is neither a
/// calibration artifact (`imatrix`) nor a multimodal vision projector
/// (`mmproj`), both of which cannot run standalone.
fn is_runnable_gguf(p: &Path) -> bool {
    if !p
        .extension()
        .map(|e| e.eq_ignore_ascii_case("gguf"))
        .unwrap_or(false)
    {
        return false;
    }
    let name = p
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    !name.contains("imatrix") && !name.contains("mmproj")
}

/// List every runnable `*.gguf` under `models_dir`: one level of repo folders
/// (`{author}--{repo}/file.gguf`, created by the hub downloader) plus any
/// GGUFs placed directly in the folder.
pub fn list_downloaded(models_dir: &Path) -> Vec<DownloadedModel> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(models_dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let repo_id = entry.file_name().to_string_lossy().replace("--", "/");
            if let Ok(files) = std::fs::read_dir(&path) {
                for f in files.flatten() {
                    let p = f.path();
                    if is_runnable_gguf(&p) {
                        let size = f.metadata().map(|m| m.len()).unwrap_or(0);
                        out.push(DownloadedModel {
                            repo_id: repo_id.clone(),
                            file_name: p
                                .file_name()
                                .map(|n| n.to_string_lossy().into_owned())
                                .unwrap_or_default(),
                            path: p.to_string_lossy().into_owned(),
                            size_bytes: size,
                        });
                    }
                }
            }
        } else if is_runnable_gguf(&path) {
            let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
            out.push(DownloadedModel {
                repo_id: "local".to_string(),
                file_name: path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                path: path.to_string_lossy().into_owned(),
                size_bytes: size,
            });
        }
    }
    out.sort_by(|a, b| {
        a.repo_id
            .cmp(&b.repo_id)
            .then_with(|| a.file_name.cmp(&b.file_name))
    });
    out
}

/// A GGUF already present on disk.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadedModel {
    pub repo_id: String,
    pub file_name: String,
    pub path: String,
    pub size_bytes: u64,
}

/// Minimal percent-encoding for query values.
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dest_path_flattens_repo_and_filename() {
        let base = Path::new("C:\\models");
        let p = dest_path(base, "bartowski/Llama-3-GGUF", "sub/dir/Q4.gguf");
        let s = p.to_string_lossy().replace('\\', "/");
        assert!(s.starts_with("C:/models/bartowski--Llama-3-GGUF/"));
        assert!(s.ends_with("sub_dir_Q4.gguf"));
    }

    #[test]
    fn urlencode_encodes_spaces_and_keeps_safe_chars() {
        assert_eq!(urlencode("qwen 2.5-coder_GGUF"), "qwen%202.5-coder_GGUF");
        assert_eq!(urlencode("safe"), "safe");
    }

    #[test]
    fn list_downloaded_scans_repo_folders_and_top_level() {
        let dir = std::env::temp_dir().join(format!("ai-hub-list-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        let repo = dir.join("acme--demo-gguf");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(repo.join("q4.gguf"), "12345").unwrap();
        std::fs::write(repo.join("notes.txt"), "ignore me").unwrap();
        std::fs::write(repo.join("calib.imatrix.gguf"), "x").unwrap();
        // mmproj projectors are not standalone models and must be ignored.
        std::fs::write(repo.join("mmproj-demo-f16.gguf"), "x").unwrap();
        // A GGUF placed directly in the folder is listed under repo "local".
        std::fs::write(dir.join("loose.gguf"), "xy").unwrap();
        std::fs::write(dir.join("mmproj-loose.gguf"), "x").unwrap();

        let found = list_downloaded(&dir);
        assert_eq!(found.len(), 2);
        let repo_hit = found
            .iter()
            .find(|m| m.repo_id == "acme/demo-gguf")
            .unwrap();
        assert_eq!(repo_hit.file_name, "q4.gguf");
        assert_eq!(repo_hit.size_bytes, 5);
        let local_hit = found.iter().find(|m| m.repo_id == "local").unwrap();
        assert_eq!(local_hit.file_name, "loose.gguf");
        assert_eq!(local_hit.size_bytes, 2);

        std::fs::remove_dir_all(&dir).ok();
    }
}
