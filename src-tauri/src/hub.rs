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

/// Search the hub for GGUF model repos. `full` + `blobs` query params make
/// the API include sibling filenames and their byte sizes.
pub async fn search(query: &str, limit: usize) -> Result<Vec<HfModel>, String> {
    let limit = limit.clamp(1, 50);
    let url = format!(
        "{HF_API_BASE}/api/models?search={}&filter=gguf&sort=downloads&direction=-1&limit={limit}&full=true&blobs=true",
        urlencode(query)
    );
    let resp = http_client()?
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

    Ok(models
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
        .collect())
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
/// `dest_path(models_dir, repo, file)`. Resumes nothing: a partial file is
/// overwritten. `on_progress` fires after every chunk (~256 KiB granularity).
pub async fn download_file(
    models_dir: &Path,
    repo_id: &str,
    file_name: &str,
    interrupt: &CancellationToken,
    mut on_progress: impl FnMut(DownloadProgress),
) -> Result<PathBuf, String> {
    let url = format!("{HF_API_BASE}/{repo_id}/resolve/main/{file_name}");
    let dest = dest_path(models_dir, repo_id, file_name);
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir failed: {e}"))?;
    }

    let resp = http_client()?
        .get(&url)
        .timeout(std::time::Duration::MAX) // large weights; no total timeout
        .send()
        .await
        .map_err(|e| format!("Download failed: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("Download returned HTTP {}", resp.status()));
    }
    let total = resp.content_length();

    let tmp = dest.with_extension("part");
    let mut file = tokio::fs::File::create(&tmp)
        .await
        .map_err(|e| format!("Failed to create `{}`: {e}", tmp.display()))?;
    let mut received: u64 = 0;
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        if interrupt.is_cancelled() {
            drop(file);
            let _ = tokio::fs::remove_file(&tmp).await;
            return Err("__cancelled__".to_string());
        }
        let chunk = chunk.map_err(|e| format!("Connection lost mid-download: {e}"))?;
        received += chunk.len() as u64;
        if received > MAX_FILE_BYTES {
            drop(file);
            let _ = tokio::fs::remove_file(&tmp).await;
            return Err("File exceeds sanity size cap".to_string());
        }
        tokio::io::AsyncWriteExt::write_all(&mut file, &chunk)
            .await
            .map_err(|e| format!("Write failed: {e}"))?;
        on_progress(DownloadProgress {
            received,
            total,
            done: false,
        });
    }
    tokio::io::AsyncWriteExt::flush(&mut file)
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
    Ok(dest)
}

/// List every completed `*.gguf` under `models_dir` (recursive, shallow depth).
pub fn list_downloaded(models_dir: &Path) -> Vec<DownloadedModel> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(models_dir) else {
        return out;
    };
    for repo in entries.flatten() {
        let repo_path = repo.path();
        if !repo_path.is_dir() {
            continue;
        }
        if let Ok(files) = std::fs::read_dir(&repo_path) {
            for f in files.flatten() {
                let p = f.path();
                if p.extension()
                    .map(|e| e.eq_ignore_ascii_case("gguf"))
                    .unwrap_or(false)
                {
                    let size = f.metadata().map(|m| m.len()).unwrap_or(0);
                    out.push(DownloadedModel {
                        repo_id: repo.file_name().to_string_lossy().replace("--", "/"),
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
    }
    out.sort_by(|a, b| a.repo_id.cmp(&b.repo_id));
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
    fn list_downloaded_scans_repo_folders() {
        let dir = std::env::temp_dir().join(format!("ai-hub-list-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        let repo = dir.join("acme--demo-gguf");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(repo.join("q4.gguf"), "12345").unwrap();
        std::fs::write(repo.join("notes.txt"), "ignore me").unwrap();
        // Stray top-level file must be ignored.
        std::fs::write(dir.join("loose.gguf"), "x").unwrap();

        let found = list_downloaded(&dir);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].repo_id, "acme/demo-gguf");
        assert_eq!(found[0].size_bytes, 5);

        std::fs::remove_dir_all(&dir).ok();
    }
}
