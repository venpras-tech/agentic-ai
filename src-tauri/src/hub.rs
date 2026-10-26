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

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

const HF_API_BASE: &str = "https://huggingface.co";
const HF_USER_AGENT: &str =
    "ai-editor/0.1 (+local desktop AI editor; huggingface-hub-client)";
/// Hard cap per model file (200 GiB) purely as a sanity guard.
const MAX_FILE_BYTES: u64 = 200 * 1024 * 1024 * 1024;
/// How often the download loop pauses to re-read the cancel/pause flags.
/// This bounds how late a user-issued cancel/pause can land.
const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);
/// Search responses are cached briefly so changing the sort control does not
/// hammer the API, and so pagination back/forward feels instant.
const SEARCH_CACHE_TTL: Duration = Duration::from_secs(120);
const TREE_CACHE_TTL: Duration = Duration::from_secs(300);

fn http_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .user_agent(HF_USER_AGENT)
        .connect_timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| format!("HTTP client error: {e}"))
}

fn with_auth(req: reqwest::RequestBuilder, token: Option<&str>) -> reqwest::RequestBuilder {
    match token.map(str::trim).filter(|t| !t.is_empty()) {
        Some(t) => req.bearer_auth(t),
        None => req,
    }
}

/// A gated/private response is the single most common hub failure: turn it into
/// an instruction the user can act on instead of a bare status code.
pub fn http_error(status: reqwest::StatusCode, what: &str) -> String {
    match status.as_u16() {
        401 | 403 => format!(
            "{what}: gated — accept the license on huggingface.co, then add a token in Settings."
        ),
        404 => format!("{what}: not found (wrong revision or file name?)"),
        429 => format!("{what}: rate limited by Hugging Face — wait a moment and retry."),
        503 => format!("{what}: Hugging Face is temporarily unavailable (503) — retry shortly."),
        _ => format!("{what} returned HTTP {status}"),
    }
}

/// One downloadable GGUF weight file inside a repository.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HfFile {
    /// File name within the repo, e.g. `model-q4_k_m.gguf`.
    pub name: String,
    pub size: Option<u64>,
    /// LFS SHA256 (when the file is stored in LFS), used for integrity checks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
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
    #[serde(default)]
    pub pipeline_tag: Option<String>,
    /// `true`, `false`, or one of HF's gated strings (`auto`, `manual`).
    #[serde(default)]
    pub gated: Option<serde_json::Value>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub last_modified: Option<String>,
    #[serde(default)]
    pub license: Option<String>,
    /// Commit SHA the listing was pinned to (`main` moves under the user).
    #[serde(default)]
    pub sha: Option<String>,
}

impl HfModel {
    pub fn is_gated(&self) -> bool {
        match &self.gated {
            Some(serde_json::Value::Bool(b)) => *b,
            Some(serde_json::Value::String(s)) => !s.is_empty() && s != "false",
            Some(serde_json::Value::Null) | None => false,
            Some(_) => true,
        }
    }
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
    #[serde(default)]
    pipeline_tag: Option<String>,
    #[serde(default)]
    gated: Option<serde_json::Value>,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    last_modified: Option<String>,
    #[serde(default)]
    sha: Option<String>,
}

#[derive(Deserialize)]
struct ApiSibling {
    rfilename: String,
    #[serde(default)]
    size: Option<u64>,
    #[serde(default)]
    lfs: Option<ApiTreeLfs>,
}

#[derive(Deserialize)]
struct ApiTreeEntry {
    path: String,
    #[serde(default)]
    size: Option<u64>,
    #[serde(default)]
    oid: Option<String>,
    #[serde(default)]
    lfs: Option<ApiTreeLfs>,
}

#[derive(Deserialize, Clone)]
struct ApiTreeLfs {
    #[serde(default)]
    size: Option<u64>,
    #[serde(default)]
    oid: Option<String>,
    #[serde(default)]
    sha256: Option<String>,
}

#[derive(Debug, Clone)]
pub struct HfTreeEntry {
    pub path: String,
    pub size: Option<u64>,
    pub sha256: Option<String>,
}

impl ApiTreeEntry {
    fn into_entry(self) -> HfTreeEntry {
        let sha256 = self
            .lfs
            .as_ref()
            .and_then(|l| l.sha256.clone().or_else(|| l.oid.clone()))
            .or(self.oid);
        let size = self.size.or_else(|| self.lfs.and_then(|l| l.size));
        HfTreeEntry {
            path: self.path,
            size,
            sha256,
        }
    }
}

fn tree_cache() -> &'static Mutex<HashMap<String, (Instant, Vec<HfTreeEntry>)>> {
    static CACHE: OnceLock<Mutex<HashMap<String, (Instant, Vec<HfTreeEntry>)>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Fetch the repo tree recursively so nested GGUFs are included. `revision` is
/// a commit SHA from the search payload when available, otherwise `main`.
pub async fn fetch_tree_entries(
    repo_id: &str,
    revision: &str,
    token: Option<&str>,
) -> Result<Vec<HfTreeEntry>, String> {
    let key = format!("{repo_id}@{revision}");
    if let Ok(cache) = tree_cache().lock() {
        if let Some((at, entries)) = cache.get(&key) {
            if at.elapsed() < TREE_CACHE_TTL {
                return Ok(entries.clone());
            }
        }
    }
    let url = format!("{HF_API_BASE}/api/models/{repo_id}/tree/{revision}?recursive=true");
    let client = http_client()?;
    let resp = with_auth(client.get(&url), token)
        .send()
        .await
        .map_err(|e| format!("HF tree failed: {e}"))?;
    if !resp.status().is_success() {
        return Err(http_error(resp.status(), "HF tree"));
    }
    let entries = resp
        .json::<Vec<ApiTreeEntry>>()
        .await
        .map_err(|e| format!("Bad HF tree response: {e}"))?
        .into_iter()
        .map(ApiTreeEntry::into_entry)
        .collect::<Vec<_>>();
    if let Ok(mut cache) = tree_cache().lock() {
        cache.insert(key, (Instant::now(), entries.clone()));
    }
    Ok(entries)
}

/// Per-repo file sizes (`/api/models/{repo_id}/tree/{revision}`). The flat
/// search API frequently reports `null` sizes for GGUF siblings, so each repo
/// that is missing sizes is back-filled here with a single cheap request.
async fn fetch_tree_sizes(
    client: &reqwest::Client,
    repo_id: &str,
    revision: &str,
    token: Option<&str>,
) -> HashMap<String, u64> {
    let url = format!("{HF_API_BASE}/api/models/{repo_id}/tree/{revision}?recursive=true");
    let Ok(resp) = with_auth(client.get(&url), token).send().await else {
        return HashMap::new();
    };
    if !resp.status().is_success() {
        return HashMap::new();
    }
    let Ok(entries) = resp.json::<Vec<ApiTreeEntry>>().await else {
        return HashMap::new();
    };
    entries
        .into_iter()
        .filter_map(|e| {
            let size = e.size.or_else(|| {
                e.lfs
                    .as_ref()
                    .and_then(|l| l.size)
            })?;
            Some((e.path, size))
        })
        .collect()
}

fn search_cache() -> &'static Mutex<HashMap<String, (Instant, Vec<HfModel>)>> {
    static CACHE: OnceLock<Mutex<HashMap<String, (Instant, Vec<HfModel>)>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Search the hub for GGUF model repos. `sort` is applied on the API where HF
/// supports it (`downloads`/`likes`), and client-side for `name`/`size`.
/// `relevance` leaves the API's own ordering untouched.
pub async fn search(
    query: &str,
    limit: usize,
    offset: usize,
    sort: &str,
    token: Option<&str>,
) -> Result<Vec<HfModel>, String> {
    let limit = limit.clamp(1, 100);
    let cache_key = format!(
        "{}|{}|{}|{}|{}",
        query.trim().to_ascii_lowercase(),
        limit,
        offset,
        sort,
        token.is_some()
    );
    if let Ok(cache) = search_cache().lock() {
        if let Some((at, models)) = cache.get(&cache_key) {
            if at.elapsed() < SEARCH_CACHE_TTL {
                return Ok(models.clone());
            }
        }
    }

    let mut url = format!(
        "{HF_API_BASE}/api/models?search={}&filter=gguf&limit={limit}&offset={offset}&full=true&blobs=true",
        urlencode(query)
    );
    match sort {
        "downloads" => url.push_str("&sort=downloads&direction=-1"),
        "likes" => url.push_str("&sort=likes&direction=-1"),
        // `relevance` and client-side sorts use the API's natural ordering.
        _ => {}
    }

    let client = http_client()?;
    let mut resp = None;
    for attempt in 0..3u32 {
        let r = with_auth(client.get(&url), token)
            .send()
            .await
            .map_err(|e| format!("HF search failed: {e}"))?;
        let status = r.status();
        if status.is_success() {
            resp = Some(r);
            break;
        }
        if attempt < 2 && (status == reqwest::StatusCode::TOO_MANY_REQUESTS || status.is_server_error()) {
            tokio::time::sleep(Duration::from_millis(500 * u64::from(attempt + 1))).await;
            continue;
        }
        return Err(http_error(status, "HF search"));
    }
    let resp = resp.ok_or_else(|| "HF search failed after retries".to_string())?;
    let models: Vec<ApiModel> = resp
        .json()
        .await
        .map_err(|e| format!("Bad HF response: {e}"))?;

    let mut out: Vec<HfModel> = models
        .into_iter()
        .map(|m| {
            let license = m
                .tags
                .iter()
                .find_map(|t| t.strip_prefix("license:").map(str::to_string));
            HfModel {
                files: m
                    .siblings
                    .into_iter()
                    .filter(|s| s.rfilename.to_ascii_lowercase().ends_with(".gguf"))
                    .map(|s| {
                        let lfs = s.lfs.clone();
                        HfFile {
                            name: s.rfilename,
                            size: s.size.or_else(|| lfs.as_ref().and_then(|l| l.size)),
                            sha256: lfs.and_then(|l| l.sha256.or(l.oid)),
                        }
                    })
                    .collect(),
                repo_id: m.id,
                author: m.author,
                likes: m.likes,
                downloads: m.downloads,
                pipeline_tag: m.pipeline_tag,
                gated: m.gated,
                tags: m.tags,
                last_modified: m.last_modified,
                license,
                sha: m.sha,
            }
        })
        .filter(|m| !m.files.is_empty())
        .collect();

    // Back-fill missing sizes for repos the search API gave us nothing for.
    let missing: Vec<(String, String)> = out
        .iter()
        .filter(|m| m.files.iter().any(|f| f.size.is_none() || f.sha256.is_none()))
        .map(|m| {
            (
                m.repo_id.clone(),
                m.sha.clone().unwrap_or_else(|| "main".to_string()),
            )
        })
        .collect();
    if !missing.is_empty() {
        use futures_util::stream;
        let sizes: Vec<(String, HashMap<String, HfTreeEntry>)> = stream::iter(missing)
            .map(|(repo, revision)| {
                let client = client.clone();
                async move {
                    let repo2 = repo.clone();
                    let map = tokio::time::timeout(
                        std::time::Duration::from_secs(10),
                        fetch_tree_sizes(&client, &repo, &revision, token),
                    )
                    .await
                    .unwrap_or_else(|_| HashMap::new())
                    .into_iter()
                    .map(|(k, v)| {
                        (
                            k.clone(),
                            HfTreeEntry {
                                path: k,
                                size: Some(v),
                                sha256: None,
                            },
                        )
                    })
                    .collect();
                    (repo2, map)
                }
            })
            .buffer_unordered(8)
            .collect()
            .await;

        let mut index: HashMap<String, HashMap<String, HfTreeEntry>> =
            sizes.into_iter().collect();
        for model in out.iter_mut() {
            if let Some(size_map) = index.remove(&model.repo_id) {
                for f in model.files.iter_mut() {
                    if f.size.is_none() {
                        f.size = size_map.get(&f.name).and_then(|e| e.size);
                    }
                }
            }
        }
    }

    if sort == "name" {
        out.sort_by(|a, b| a.repo_id.to_ascii_lowercase().cmp(&b.repo_id.to_ascii_lowercase()));
    } else if sort == "size" {
        out.sort_by(|a, b| {
            let sa: u64 = a.files.iter().filter_map(|f| f.size).sum();
            let sb: u64 = b.files.iter().filter_map(|f| f.size).sum();
            sb.cmp(&sa)
        });
    }

    if let Ok(mut cache) = search_cache().lock() {
        if cache.len() > 64 {
            cache.clear();
        }
        cache.insert(cache_key, (Instant::now(), out.clone()));
    }
    Ok(out)
}

/// Repo metadata (`/api/models/{repo}`) for the agent's `hf_model_info` tool
/// and for discovering a GGUF repo's `base_model` tokenizer source.
pub async fn model_info(repo_id: &str, token: Option<&str>) -> Result<serde_json::Value, String> {
    let url = format!("{HF_API_BASE}/api/models/{repo_id}");
    let client = http_client()?;
    let resp = with_auth(client.get(&url), token)
        .send()
        .await
        .map_err(|e| format!("HF model info failed: {e}"))?;
    if !resp.status().is_success() {
        return Err(http_error(resp.status(), "HF model info"));
    }
    resp.json::<serde_json::Value>()
        .await
        .map_err(|e| format!("Bad HF model info: {e}"))
}

/// Extract `base_model` entries from a model-info payload. HF exposes them as
/// `cardData.base_model` (string/array) and as `base_model:*` tags.
fn base_models(info: &serde_json::Value) -> Vec<String> {
    let mut out = Vec::new();
    let mut push = |v: &serde_json::Value| {
        if let Some(s) = v.as_str() {
            if !s.trim().is_empty() {
                out.push(s.trim().to_string());
            }
        }
    };
    if let Some(card) = info.get("cardData") {
        for key in ["base_model", "base_models"] {
            match card.get(key) {
                Some(serde_json::Value::Array(items)) => {
                    for item in items {
                        if let Some(name) = item.get("id").or_else(|| item.get("name")) {
                            push(name);
                        } else {
                            push(item);
                        }
                    }
                }
                Some(v) => push(v),
                None => {}
            }
        }
    }
    if let Some(tags) = info.get("tags").and_then(|t| t.as_array()) {
        for tag in tags {
            if let Some(s) = tag.as_str() {
                if let Some(rest) = s.strip_prefix("base_model:") {
                    if !rest.trim().is_empty() {
                        out.push(rest.trim().to_string());
                    }
                }
            }
        }
    }
    out.sort();
    out.dedup();
    out
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

/// Parse a split-GGUF shard filename (`…-00002-of-00003.gguf`) into the 1-based
/// part number and the total part count. Returns `None` for ordinary files.
pub fn parse_shard(file_name: &str) -> Option<(u32, u32)> {
    let base = file_name.rsplit(['/', '\\']).next().unwrap_or(file_name);
    let lower = base.to_ascii_lowercase();
    let rest = lower.strip_suffix(".gguf")?;
    let (head, total) = rest.rsplit_once("-of-")?;
    let (stem, part) = head.rsplit_once('-')?;
    if stem.is_empty() || part.len() != 5 || total.len() != 5 {
        return None;
    }
    let part: u32 = part.parse().ok()?;
    let total: u32 = total.parse().ok()?;
    if part == 0 || total < 2 || part > total {
        return None;
    }
    Some((part, total))
}

/// Rewrite the 5-digit part field of a shard filename, preserving the prefix
/// (including any subdirectory) and the total: `m-00003-of-00003.gguf` with
/// `k = 1` → `m-00001-of-00003.gguf`. Returns `file_name` unchanged when it is
/// not a recognised shard name.
pub fn replace_shard_part(file_name: &str, k: u32, _total: u32) -> String {
    let Some((_, real_total)) = parse_shard(file_name) else {
        return file_name.to_string();
    };
    // A shard suffix is always `-NNNNN-of-NNNNN.gguf`, so its length is fixed.
    let cut = format!("-{:05}-of-{:05}.gguf", 1, real_total).len();
    let prefix = &file_name[..file_name.len() - cut];
    format!("{prefix}-{k:05}-of-{real_total:05}.gguf")
}

/// Every shard filename of the split model that `file_name` belongs to.
///
/// llama.cpp can only open the **first** shard (`…-00001-of-0000N.gguf`) and
/// requires every sibling in the same folder, so downloading a single shard
/// always produces an unloadable model. Given any part of a split, this returns
/// all parts in ascending order; for a single-file model it returns just that
/// file. The original prefix (including any subdirectory and its casing) is
/// preserved, only the 5-digit part field is rewritten.
pub fn shard_group(file_name: &str) -> Vec<String> {
    let Some((_, total)) = parse_shard(file_name) else {
        return vec![file_name.to_string()];
    };
    (1..=total)
        .map(|k| replace_shard_part(file_name, k, total))
        .collect()
}

/// Best-effort fetch of a repo's `tokenizer.json` into its folder so the
/// orchestrator can register exact token counts for a downloaded model.
///
/// Bartowski-style GGUF repos often do not ship a tokenizer; when the repo
/// itself 404s this follows the model card's `base_model:` and fetches the
/// tokenizer from the upstream instruct model instead. Tolerant: no tokenizer
/// anywhere simply returns `false` rather than failing the model download.
pub async fn download_tokenizer(
    models_dir: &Path,
    repo_id: &str,
    token: Option<&str>,
) -> bool {
    let dest = repo_dir(models_dir, repo_id).join("tokenizer.json");
    if dest.is_file() {
        return true;
    }
    let client = match http_client() {
        Ok(c) => c,
        Err(_) => return false,
    };
    let mut candidates = vec![repo_id.to_string()];
    if let Ok(info) = model_info(repo_id, token).await {
        candidates.extend(base_models(&info));
    }
    for candidate in candidates {
        let url = format!("{HF_API_BASE}/{candidate}/resolve/main/tokenizer.json");
        let resp = match with_auth(client.get(&url), token)
            .timeout(std::time::Duration::from_secs(60))
            .send()
            .await
        {
            Ok(r) if r.status().is_success() => r,
            _ => continue,
        };
        let bytes = match resp.bytes().await {
            Ok(b) if !b.is_empty() => b,
            _ => continue,
        };
        // tokenizer.json is a few MB at most; guard against a misrouted URL.
        if bytes.len() > 50 * 1024 * 1024 {
            continue;
        }
        let _ = std::fs::create_dir_all(repo_dir(models_dir, repo_id));
        if tokio::fs::write(&dest, &bytes).await.is_ok() {
            return true;
        }
    }
    false
}

/// SHA256 of an on-disk file, for LFS integrity verification.
pub async fn file_sha256(path: &Path) -> Result<String, String> {
    use sha2::{Digest, Sha256};
    let mut file = tokio::fs::File::open(path)
        .await
        .map_err(|e| format!("open {}: {e}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1024 * 1024];
    loop {
        use tokio::io::AsyncReadExt;
        let n = file
            .read(&mut buf)
            .await
            .map_err(|e| format!("read {}: {e}", path.display()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// Expected SHA256 for `file_name` in `repo_id` at `revision`, using the same
/// tree metadata the downloader already fetches.
pub async fn expected_sha256(
    repo_id: &str,
    file_name: &str,
    revision: &str,
    token: Option<&str>,
) -> Option<String> {
    fetch_tree_entries(repo_id, revision, token)
        .await
        .ok()?
        .into_iter()
        .find(|e| e.path == file_name)
        .and_then(|e| e.sha256)
}

/// Progress callback payload handed to `download_file`.
pub struct DownloadProgress {
    pub received: u64,
    pub total: Option<u64>,
    /// True once the file is fully written and flushed.
    pub done: bool,
}

/// Stream `https://huggingface.co/{repo}/resolve/{revision}/{file}` into
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
    revision: &str,
    token: Option<&str>,
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

    let revision = if revision.trim().is_empty() {
        "main"
    } else {
        revision.trim()
    };
    let encoded_file = file_name
        .split('/')
        .map(urlencode)
        .collect::<Vec<_>>()
        .join("/");
    let url = format!("{HF_API_BASE}/{repo_id}/resolve/{revision}/{encoded_file}");
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
        let mut req = with_auth(http_client()?.get(&url), token).timeout(Duration::MAX);
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
            return Err(http_error(code, "Download"));
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
/// calibration artifact (`imatrix`) nor a multimodal vision projector. The
/// projector check uses filename-token rules instead of a plain substring so a
/// real LLM whose name merely contains `vision`/`vit` is not excluded.
pub fn is_runnable_gguf(p: &Path) -> bool {
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
    if name.contains("imatrix") {
        return false;
    }
    // Projector files are named `mmproj-*` (sometimes `*-mmproj-*`), or start
    // with the vision tower family name. Prefix/suffix rules only: `vision` in
    // the middle of a model name is legitimate.
    if name.starts_with("mmproj") || name.contains("-mmproj") {
        return false;
    }
    for prefix in ["clip-", "clip_", "vit-", "vit_", "siglip-", "siglip_"] {
        if name.starts_with(prefix) {
            return false;
        }
    }
    let stem = name.trim_end_matches(".gguf");
    let tokens: Vec<&str> = stem.split(['-', '_', '.']).collect();
    if tokens
        .iter()
        .any(|t| matches!(*t, "projector" | "mmproj" | "siglip" | "vit"))
    {
        return false;
    }
    true
}

/// Companion projector files that a vision model needs alongside its weights.
pub fn companion_ggufs(files: &[HfFile]) -> Vec<HfFile> {
    files
        .iter()
        .filter(|f| {
            let name = f.name.to_ascii_lowercase();
            name.ends_with(".gguf")
                && (name.starts_with("mmproj")
                    || name.contains("-mmproj")
                    || name.starts_with("clip-")
                    || name.starts_with("vit-")
                    || name.starts_with("siglip-"))
        })
        .cloned()
        .collect()
}

/// List every runnable `*.gguf` under `models_dir`: one level of repo folders
/// (`{author}--{repo}/file.gguf`, created by the hub downloader) plus any
/// GGUFs placed directly in the folder. Also recursively scans nested
/// subdirectories so models placed inside extra folders are discovered.
pub fn list_downloaded(models_dir: &Path) -> Vec<DownloadedModel> {
    fn walk(
        base: &Path,
        models_dir: &Path,
        repo_id: Option<String>,
        out: &mut Vec<DownloadedModel>,
    ) {
        let Ok(entries) = std::fs::read_dir(base) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let name = entry.file_name().to_string_lossy().into_owned();
                // If we're at the top level and this looks like a repo folder, derive repo_id.
                if repo_id.is_none() && name.contains("--") {
                    let derived = name.replace("--", "/");
                    walk(&path, models_dir, Some(derived), out);
                } else if repo_id.is_none() {
                    // Could be a folder containing models directly; treat as "local" unless it looks like repo.
                    walk(&path, models_dir, Some("local".to_string()), out);
                } else {
                    walk(&path, models_dir, repo_id.clone(), out);
                }
            } else if is_runnable_gguf(&path) {
                let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
                out.push(DownloadedModel {
                    repo_id: repo_id.clone().unwrap_or_else(|| "local".to_string()),
                    file_name: path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                    path: path.to_string_lossy().into_owned(),
                    size_bytes: size,
                });
            }
        }
    }

    let mut out = Vec::new();
    walk(models_dir, models_dir, None, &mut out);
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
    fn parse_shard_reads_part_and_total() {
        assert_eq!(parse_shard("qwen3-4b-q4_k_m-00003-of-00003.gguf"), Some((3, 3)));
        assert_eq!(parse_shard("a/b/model-00001-of-00004.gguf"), Some((1, 4)));
        assert_eq!(parse_shard("MODEL-00012-OF-00012.GGUF"), Some((12, 12)));
        assert_eq!(parse_shard("plain-q4_k_m.gguf"), None);
        assert_eq!(parse_shard("model-of-00003.gguf"), None);
        assert_eq!(parse_shard("model-00000-of-00003.gguf"), None);
        assert_eq!(parse_shard("model-00004-of-00003.gguf"), None);
        assert_eq!(parse_shard("model-3-of-3.gguf"), None);
    }

    #[test]
    fn shard_group_expands_every_part_in_order() {
        // Any part resolves to the full ordered set — this is what stops the
        // "only shard 3 downloaded" failure.
        let all = shard_group("qwen3-4b-q4_k_m-00003-of-00003.gguf");
        assert_eq!(
            all,
            vec![
                "qwen3-4b-q4_k_m-00001-of-00003.gguf",
                "qwen3-4b-q4_k_m-00002-of-00003.gguf",
                "qwen3-4b-q4_k_m-00003-of-00003.gguf",
            ]
        );
        // A single-file model is its own group.
        assert_eq!(shard_group("model-q4_k_m.gguf"), vec!["model-q4_k_m.gguf"]);
        // Subdirectories and prefix casing survive; only the part field changes.
        assert_eq!(
            shard_group("sub/dir/Big-00002-of-00002.gguf"),
            vec!["sub/dir/Big-00001-of-00002.gguf", "sub/dir/Big-00002-of-00002.gguf"]
        );
    }

    #[test]
    fn list_downloaded_scans_repo_folders_and_top_level() {        let dir = std::env::temp_dir().join(format!("ai-hub-list-{}", std::process::id()));
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

    #[test]
    fn projector_detection_uses_token_rules_not_substrings() {
        assert!(!is_runnable_gguf(Path::new("mmproj-model-f16.gguf")));
        assert!(!is_runnable_gguf(Path::new("model-mmproj-f16.gguf")));
        assert!(!is_runnable_gguf(Path::new("clip-vit-large.gguf")));
        assert!(!is_runnable_gguf(Path::new("siglip-so400m.gguf")));
        assert!(!is_runnable_gguf(Path::new("weights.imatrix.gguf")));
        // A real LLM with `vision` (or even `vit` inside a word) in its name is
        // still runnable - substring matching used to hide these.
        assert!(is_runnable_gguf(Path::new("llava-v1.5-7b-vision-q4_k_m.gguf")));
        assert!(is_runnable_gguf(Path::new("qwen2.5-vl-7b-instruct-q4.gguf")));
        assert!(is_runnable_gguf(Path::new("vitamin-coder-7b-q4.gguf")));
        assert!(is_runnable_gguf(Path::new("qwen3-4b-q4_k_m.gguf")));
    }

    #[test]
    fn companion_ggufs_finds_vision_projectors_only() {
        let files = vec![
            HfFile {
                name: "model-q4_k_m.gguf".into(),
                size: Some(1),
                sha256: None,
            },
            HfFile {
                name: "mmproj-model-f16.gguf".into(),
                size: Some(2),
                sha256: None,
            },
            HfFile {
                name: "README.md".into(),
                size: None,
                sha256: None,
            },
        ];
        let companions = companion_ggufs(&files);
        assert_eq!(companions.len(), 1);
        assert_eq!(companions[0].name, "mmproj-model-f16.gguf");
    }

    #[test]
    fn base_models_reads_card_and_tags() {
        let info = serde_json::json!({
            "cardData": { "base_model": ["meta-llama/Llama-3.1-8B-Instruct"] },
            "tags": ["base_model:Qwen/Qwen2.5-7B-Instruct", "gguf"]
        });
        let bases = base_models(&info);
        assert!(bases.contains(&"meta-llama/Llama-3.1-8B-Instruct".to_string()));
        assert!(bases.contains(&"Qwen/Qwen2.5-7B-Instruct".to_string()));
    }

    #[test]
    fn http_errors_are_actionable() {
        assert!(http_error(reqwest::StatusCode::UNAUTHORIZED, "Download").contains("gated"));
        assert!(http_error(reqwest::StatusCode::FORBIDDEN, "HF search").contains("Settings"));
        assert!(http_error(reqwest::StatusCode::TOO_MANY_REQUESTS, "HF search").contains("rate limited"));
        assert!(http_error(reqwest::StatusCode::NOT_FOUND, "HF tree").contains("revision"));
        assert!(http_error(reqwest::StatusCode::BAD_GATEWAY, "x").contains("502"));
    }

    #[tokio::test]
    async fn file_sha256_matches_known_digest() {
        let path = std::env::temp_dir().join(format!("ai-hub-sha-{}.bin", std::process::id()));
        tokio::fs::write(&path, b"abc").await.unwrap();
        let digest = file_sha256(&path).await.unwrap();
        assert_eq!(
            digest,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        let _ = tokio::fs::remove_file(&path).await;
    }
}
