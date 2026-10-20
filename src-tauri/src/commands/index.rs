//! Incremental workspace text index + search (`index_status` / `index_rebuild`
//! / `index_search` / `index_semantic_search`).
//!
//! `index_rebuild` walks the workspace with the `ignore` crate so `.gitignore`
//! rules (plus hidden-file skipping) apply for free. Rebuilds are incremental:
//! files whose size + mtime are unchanged are reused from the in-memory cache
//! instead of being re-read. `index_search` runs a case-insensitive regex (with
//! a literal-substring fallback) over the indexed files. Semantic search is
//! pluggable behind the [`SemanticSearchBackend`] trait: the default is a cheap
//! keyword scorer ([`KeywordBackend`]); an embeddings backend can be installed
//! behind the same interface (see the `embedder` Cargo feature + the
//! `EmbedderBackend` slot).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use regex::Regex;
use serde::Serialize;
use tauri::{Emitter, State};

/// Skip files larger than this — indexing is for navigation, not bulk.
const MAX_FILE_BYTES: u64 = 1_000_000;
/// Skip reads once this many files are indexed (safety valve for huge repos).
const MAX_FILES: usize = 200_000;
/// Width of the match preview put in the results.
const MAX_PREVIEW_CHARS: usize = 240;
/// Default cap on results returned by one search.
const DEFAULT_MAX_RESULTS: usize = 100;

/// One oversized file's in-memory representation.
#[derive(Debug, Clone)]
struct IndexedFile {
    rel_path: String,
    size: u64,
    modified_at: i128,
    content: String,
}

#[derive(Default)]
struct IndexState {
    files: HashMap<String, IndexedFile>,
    total_chars: usize,
    workspace: Option<PathBuf>,
    last_built: Option<String>,
}

/// A document handed to a [`SemanticSearchBackend`]. Borrows the index, so the
/// trait stays backend-agnostic (no leak of internal index types).
pub struct SemDoc<'a> {
    /// Workspace-relative path (forward slashes).
    pub rel_path: &'a str,
    /// Full file text.
    pub content: &'a str,
}

/// Pluggable semantic-search backend. The default [`KeywordBackend`] scores
/// term frequencies; an embedder-backed implementation slots in behind the same
/// interface (see the `embedder` feature) via
/// [`SearchIndex::set_semantic_backend`].
pub trait SemanticSearchBackend: Send + Sync {
    /// Backend label for logs / diagnostics.
    #[allow(dead_code)]
    fn name(&self) -> &'static str;
    /// Return up to `cap` hits, best first. Hits use the shared 1-based
    /// `line_number` + [`preview`] formatting.
    fn search(&self, docs: &[SemDoc<'_>], query: &str, cap: usize) -> Vec<IndexSearchHit>;
}

/// Default backend: coarse keyword/TF scorer (term-frequency × first matching
/// line). See [`SearchIndex::semantic_search`] for the behavior contract.
pub struct KeywordBackend;

impl SemanticSearchBackend for KeywordBackend {
    fn name(&self) -> &'static str {
        "keyword"
    }

    fn search(&self, docs: &[SemDoc<'_>], query: &str, cap: usize) -> Vec<IndexSearchHit> {
        let terms: Vec<String> = query
            .split_whitespace()
            .filter(|t| t.len() >= 3)
            .map(|t| t.to_lowercase())
            .collect();
        if terms.is_empty() {
            return Vec::new();
        }
        let mut scored: Vec<(i64, IndexSearchHit)> = Vec::new();
        for doc in docs {
            let lower = doc.content.to_lowercase();
            let mut score = 0i64;
            let mut hit: Option<(usize, String)> = None;
            for term in &terms {
                let count = lower.matches(term).count() as i64;
                if count == 0 {
                    continue;
                }
                score += count;
                if hit.is_none() {
                    for (idx, line_ref) in doc.content.lines().enumerate() {
                        if line_ref.to_lowercase().contains(term) {
                            hit = Some((idx + 1, line_ref.to_string()));
                            break;
                        }
                    }
                }
            }
            if score > 0 && hit.is_some() {
                let (line_number, line) = hit.unwrap();
                scored.push((
                    score,
                    IndexSearchHit {
                        path: doc.rel_path.to_string(),
                        line: preview(&line),
                        line_number,
                    },
                ));
            }
        }
        scored.sort_by(|a, b| b.0.cmp(&a.0));
        scored.into_iter().take(cap).map(|(_, hit)| hit).collect()
    }
}

/// Embedder-slot placeholder behind the `embedder` feature. It currently
/// delegates to the keyword scorer so the feature compiles end-to-end; wiring a
/// real local embedding model is the follow-up that the trait makes possible.
#[cfg(feature = "embedder")]
pub struct EmbedderBackend;

#[cfg(feature = "embedder")]
impl SemanticSearchBackend for EmbedderBackend {
    fn name(&self) -> &'static str {
        "embedder"
    }

    fn search(&self, docs: &[SemDoc<'_>], query: &str, cap: usize) -> Vec<IndexSearchHit> {
        KeywordBackend.search(docs, query, cap)
    }
}

/// Pick the built-in backend for a fresh [`SearchIndex`].
fn default_semantic_backend() -> Box<dyn SemanticSearchBackend> {
    #[cfg(feature = "embedder")]
    {
        Box::new(EmbedderBackend)
    }
    #[cfg(not(feature = "embedder"))]
    {
        Box::new(KeywordBackend)
    }
}

/// Tauri-managed handle to the in-process index.
pub struct SearchIndex {
    inner: Mutex<IndexState>,
    semantic: Mutex<Box<dyn SemanticSearchBackend>>,
}

impl Default for SearchIndex {
    fn default() -> Self {
        Self {
            inner: Mutex::new(IndexState::default()),
            semantic: Mutex::new(default_semantic_backend()),
        }
    }
}

impl SearchIndex {
    /// Install a different semantic-search backend (e.g. an embedder behind the
    /// `embedder` feature). Replaces whatever backend is currently installed.
    /// Unreferenced until embedder wiring lands, so it stays part of the
    /// documented extension surface rather than dead generic code.
    #[allow(dead_code)]
    pub fn set_semantic_backend(&self, backend: Box<dyn SemanticSearchBackend>) {
        *self.semantic.lock().unwrap() = backend;
    }
}

/// Snapshot of the index for the UI.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexStatus {
    pub indexed_files: usize,
    pub total_chars: usize,
    pub workspace: Option<String>,
    pub last_built: Option<String>,
}

/// One `path:line` match surfaced to the UI.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexSearchHit {
    pub path: String,
    pub line: String,
    pub line_number: usize,
}

impl SearchIndex {
    /// Rebuild the index for `workspace`, reusing unchanged files. Returns the
    /// post-build status.
    pub fn rebuild(&self, workspace: &Path) -> Result<IndexStatus, String> {
        let mut st = self.inner.lock().unwrap();
        Self::rebuild_inner(workspace, &mut st)
    }

    fn rebuild_inner(workspace: &Path, st: &mut IndexState) -> Result<IndexStatus, String> {
        let mut next: HashMap<String, IndexedFile> = HashMap::new();
        let mut total_chars = 0usize;
        let walker = ignore::WalkBuilder::new(workspace)
            .git_ignore(true)
            .git_global(true)
            .git_exclude(true)
            .parents(true)
            .require_git(false)
            .build();
        for entry in walker {
            let Ok(entry) = entry else { continue };
            if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
                continue;
            }
            if next.len() >= MAX_FILES {
                break;
            }
            let path = entry.path();
            let Ok(rel) = path.strip_prefix(workspace) else {
                continue;
            };
            let rel_str = rel.to_string_lossy().replace('\\', "/");
            let is_hidden = rel.components().any(|c| {
                matches!(c, std::path::Component::Normal(os) if os.to_string_lossy().starts_with('.'))
            });
            if is_hidden {
                continue;
            }
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            let size = meta.len();
            let modified_at = modified_ms(&meta);
            if let Some(existing) = st.files.get(&rel_str) {
                if existing.size == size && existing.modified_at == modified_at {
                    total_chars += existing.content.len();
                    next.insert(rel_str, existing.clone());
                    continue;
                }
            }
            if size > MAX_FILE_BYTES {
                continue;
            }
            let Ok(bytes) = std::fs::read(path) else {
                continue;
            };
            if is_binary(&bytes) {
                continue;
            }
            let content = String::from_utf8_lossy(&bytes).into_owned();
            total_chars += content.len();
            next.insert(
                rel_str.clone(),
                IndexedFile {
                    rel_path: rel_str,
                    size,
                    modified_at,
                    content,
                },
            );
        }
        st.files = next;
        st.total_chars = total_chars;
        st.workspace = Some(workspace.to_path_buf());
        st.last_built = Some(now_iso());
        Ok(status_from(st))
    }

    /// Search the indexed files with a case-insensitive regex, falling back to
    /// an escaped literal substring match when the query isn't valid regex.
    /// Lazily builds the index for `workspace` when it is empty or stale.
    pub fn search(
        &self,
        workspace: &Path,
        query: &str,
        max_results: Option<usize>,
    ) -> Result<Vec<IndexSearchHit>, String> {
        let mut st = self.inner.lock().unwrap();
        if st.workspace.as_ref().map(|p| p.as_os_str()) != Some(workspace.as_os_str())
            || st.files.is_empty()
        {
            Self::rebuild_inner(workspace, &mut st)?;
        }
        let query = query.trim();
        if query.is_empty() {
            return Ok(Vec::new());
        }
        let needle = Regex::new(&format!("(?i){}", regex_escape_or_raw(query)));
        let cap = max_results.unwrap_or(DEFAULT_MAX_RESULTS).min(1000).max(1);
        let mut out: Vec<IndexSearchHit> = Vec::new();
        for file in st.files.values() {
            for (idx, line_ref) in file.content.lines().enumerate() {
                let matched = match &needle {
                    Ok(re) => re.is_match(line_ref),
                    Err(_) => line_ref.to_lowercase().contains(&query.to_lowercase()),
                };
                if !matched {
                    continue;
                }
                out.push(IndexSearchHit {
                    path: file.rel_path.clone(),
                    line: preview(line_ref),
                    line_number: idx + 1,
                });
                if out.len() >= cap {
                    return Ok(out);
                }
            }
        }
        Ok(out)
    }

    /// Semantic search over the indexed files, delegated to the installed
    /// [`SemanticSearchBackend`] (default [`KeywordBackend`]). Lazily builds the
    /// index for `workspace` when it is empty or stale.
    pub fn semantic_search(
        &self,
        workspace: &Path,
        query: &str,
        max_results: Option<usize>,
    ) -> Result<Vec<IndexSearchHit>, String> {
        let mut st = self.inner.lock().unwrap();
        if st.workspace.as_ref().map(|p| p.as_os_str()) != Some(workspace.as_os_str())
            || st.files.is_empty()
        {
            Self::rebuild_inner(workspace, &mut st)?;
        }
        let query = query.trim();
        let cap = max_results.unwrap_or(DEFAULT_MAX_RESULTS).min(1000).max(1);
        if query.is_empty() {
            return Ok(Vec::new());
        }
        let docs: Vec<SemDoc<'_>> = st
            .files
            .iter()
            .map(|(rel, f)| SemDoc {
                rel_path: rel,
                content: &f.content,
            })
            .collect();
        let backend = self.semantic.lock().unwrap();
        Ok(backend.search(&docs, query, cap))
    }

    /// Current status without rebuilding.
    pub fn status(&self) -> IndexStatus {
        let st = self.inner.lock().unwrap();
        status_from(&st)
    }

    /// Incremental update for a set of relative paths that changed on disk
    /// (create / modify / delete), keeping `st.files` + `st.total_chars`
    /// consistent. No-op when the index currently covers a different
    /// workspace. Returns the post-update status when anything changed.
    pub fn update_paths(&self, workspace: &Path, rel_paths: &[String]) -> Option<IndexStatus> {
        let mut st = self.inner.lock().unwrap();
        if st.workspace.as_ref().map(|p| p.as_os_str()) != Some(workspace.as_os_str()) {
            return None;
        }
        let mut changed = false;
        for rel in rel_paths {
            let rel_str = rel.replace('\\', "/");
            if rel_str.is_empty() || rel_str.split('/').any(|seg| seg.starts_with('.')) {
                continue;
            }
            let full = workspace.join(&rel_str);
            let meta = match std::fs::metadata(&full) {
                Ok(m) => m,
                Err(_) => {
                    if let Some(prev) = st.files.remove(&rel_str) {
                        st.total_chars = st.total_chars.saturating_sub(prev.content.len());
                        changed = true;
                    }
                    continue;
                }
            };
            if !meta.is_file() {
                continue;
            }
            let size = meta.len();
            let modified_at = modified_ms(&meta);
            let unchanged = st
                .files
                .get(&rel_str)
                .is_some_and(|e| e.size == size && e.modified_at == modified_at);
            if unchanged {
                continue;
            }
            if size > MAX_FILE_BYTES {
                if let Some(prev) = st.files.remove(&rel_str) {
                    st.total_chars = st.total_chars.saturating_sub(prev.content.len());
                }
                changed = true;
                continue;
            }
            let Ok(bytes) = std::fs::read(&full) else {
                continue;
            };
            if is_binary(&bytes) {
                if let Some(prev) = st.files.remove(&rel_str) {
                    st.total_chars = st.total_chars.saturating_sub(prev.content.len());
                }
                changed = true;
                continue;
            }
            let content = String::from_utf8_lossy(&bytes).into_owned();
            let prev_len = st.files.get(&rel_str).map(|f| f.content.len()).unwrap_or(0);
            st.total_chars = st.total_chars.saturating_sub(prev_len) + content.len();
            st.files.insert(
                rel_str.clone(),
                IndexedFile {
                    rel_path: rel_str.clone(),
                    size,
                    modified_at,
                    content,
                },
            );
            changed = true;
        }
        if changed {
            st.last_built = Some(now_iso());
            Some(status_from(&st))
        } else {
            None
        }
    }
}

fn status_from(st: &IndexState) -> IndexStatus {
    IndexStatus {
        indexed_files: st.files.len(),
        total_chars: st.total_chars,
        workspace: st
            .workspace
            .as_ref()
            .map(|p| p.to_string_lossy().to_string()),
        last_built: st.last_built.clone(),
    }
}

/// Truncate a matched line to a preview-friendly length, marking elision.
fn preview(line: &str) -> String {
    let trimmed = line.trim();
    if trimmed.chars().count() <= MAX_PREVIEW_CHARS {
        return trimmed.to_string();
    }
    let head: String = trimmed.chars().take(MAX_PREVIEW_CHARS).collect();
    format!("{head}…")
}

/// Treat a query as regex when it parses, else escape it so a plain-text search
/// never throws. `Regex::new` is quicker than separating the cases and keeps
/// the safety guarantee for free.
fn regex_escape_or_raw(query: &str) -> String {
    if Regex::new(query).is_ok() {
        query.to_string()
    } else {
        regex::escape(query)
    }
}

/// A NUL byte in the leading bytes marks the file as binary.
fn is_binary(bytes: &[u8]) -> bool {
    bytes.iter().take(8192).any(|b| *b == 0)
}

fn modified_ms(meta: &std::fs::Metadata) -> i128 {
    match meta.modified() {
        Ok(m) => match m.duration_since(std::time::UNIX_EPOCH) {
            Ok(d) => d.as_millis() as i128,
            Err(_) => 0,
        },
        Err(_) => 0,
    }
}

fn now_iso() -> String {
    chrono::Local::now().to_rfc3339()
}

/// Current index status for the workspace (no rebuild).
#[tauri::command]
pub async fn index_status(state: State<'_, Arc<SearchIndex>>) -> Result<IndexStatus, String> {
    Ok(state.status())
}

/// (Re)build the incremental text index for `workspace`.
#[tauri::command]
pub async fn index_rebuild(
    app: tauri::AppHandle,
    state: State<'_, Arc<SearchIndex>>,
    workspace: String,
) -> Result<IndexStatus, String> {
    let ws = PathBuf::from(&workspace);
    if !ws.is_dir() {
        return Err(format!("Not a folder: {workspace}"));
    }
    let _ = app.emit(
        "index://progress",
        serde_json::json!({ "phase": "start", "workspace": workspace }),
    );
    let status = state.rebuild(&ws)?;
    let _ = app.emit(
        "index://progress",
        serde_json::json!({ "phase": "done", "status": status }),
    );
    Ok(status)
}

/// Regex (or literal) search over the workspace index.
#[tauri::command]
pub async fn index_search(
    state: State<'_, Arc<SearchIndex>>,
    workspace: String,
    query: String,
    max_results: Option<usize>,
) -> Result<Vec<IndexSearchHit>, String> {
    state.search(Path::new(&workspace), &query, max_results)
}

/// Keyword-fallback stub for semantic search (real embeddings are out of
/// scope); returns the top files by raw term frequency.
#[tauri::command]
pub async fn index_semantic_search(
    state: State<'_, Arc<SearchIndex>>,
    workspace: String,
    query: String,
    max_results: Option<usize>,
) -> Result<Vec<IndexSearchHit>, String> {
    state.semantic_search(Path::new(&workspace), &query, max_results)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binary_detection_flags_nul_and_not_text() {
        assert!(is_binary(&[0u8, 1, 2, 3]));
        assert!(!is_binary(b"hello world\nplain text"));
    }

    #[test]
    fn preview_truncates_long_lines_with_ellipsis() {
        let short = "let x = 1;";
        assert_eq!(preview(short), short);
        let long = "x".repeat(300);
        let p = preview(&long);
        assert!(p.ends_with('…'));
        assert!(p.chars().count() <= MAX_PREVIEW_CHARS + 2);
    }

    #[test]
    fn bad_regex_uses_literal_fallback() {
        // `(` alone is invalid regex → escaped into a literal substring search.
        assert_eq!(regex_escape_or_raw("("), "\\(");
        assert_eq!(regex_escape_or_raw("tokio::task"), "tokio::task");
    }

    #[test]
    fn rebuilt_index_searches_incrementally() {
        let ws = std::env::temp_dir().join(format!("ai-editor-idx-{}", std::process::id()));
        std::fs::create_dir_all(&ws).unwrap();
        std::fs::write(ws.join("a.txt"), "apple banana\nbanana split").unwrap();
        std::fs::write(ws.join("b.txt"), "cherry\n").unwrap();
        std::fs::write(ws.join("node_modules_placeholder.md"), "banana\n").unwrap();
        let idx = SearchIndex::default();
        let st = idx.rebuild(&ws).unwrap();
        assert_eq!(
            st.indexed_files, 3,
            "no .gitignore here, so all three files index"
        );
        let hits = idx.search(&ws, "banana", Some(10)).unwrap();
        assert_eq!(hits.len(), 3, "banana lines: a.txt x2 + placeholder x1");
        let mut by_path: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
        for h in &hits {
            *by_path.entry(h.path.as_str()).or_default() += 1;
        }
        assert_eq!(by_path.get("a.txt"), Some(&2));
        assert_eq!(by_path.get("node_modules_placeholder.md"), Some(&1));
        // Rebuild is still complete data-wise (incremental reuse is a perf
        // optimization, not a correctness change).
        let st2 = idx.rebuild(&ws).unwrap();
        assert_eq!(st2.indexed_files, 3);
        assert_eq!(idx.search(&ws, "cherry", None).unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn rebuild_relies_on_gitignore_when_present() {
        let ws = std::env::temp_dir().join(format!("ai-editor-idx-g-{}", std::process::id()));
        std::fs::create_dir_all(ws.join("vendor")).unwrap();
        std::fs::write(ws.join(".gitignore"), "vendor/\n").unwrap();
        std::fs::write(ws.join("src.txt"), "keep me\n").unwrap();
        std::fs::write(ws.join("vendor/deps.txt"), "drop me\n").unwrap();
        let idx = SearchIndex::default();
        let st = idx.rebuild(&ws).unwrap();
        assert_eq!(st.indexed_files, 1, ".gitignore pruned vendor/");
        assert_eq!(idx.search(&ws, "drop", None).unwrap().len(), 0);
        assert_eq!(idx.search(&ws, "keep", None).unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn stale_or_missing_workspace_index_is_rebuilt_on_search() {
        let ws = std::env::temp_dir().join(format!("ai-editor-idx-lazy-{}", std::process::id()));
        std::fs::create_dir_all(&ws).unwrap();
        std::fs::write(ws.join("x.txt"), "lazy loaded\n").unwrap();
        let idx = SearchIndex::default();
        // No explicit rebuild: search builds on demand.
        let hits = idx.search(&ws, "lazy", None).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(idx.status().indexed_files, 1);
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn semantic_search_is_keyword_fall_stub() {
        let ws = std::env::temp_dir().join(format!("ai-editor-idx-sem-{}", std::process::id()));
        std::fs::create_dir_all(&ws).unwrap();
        std::fs::write(ws.join("a.txt"), "the red fox jumps\n").unwrap();
        std::fs::write(ws.join("b.txt"), "no query terms here\n").unwrap();
        let idx = SearchIndex::default();
        let hits = idx.semantic_search(&ws, "red fox", Some(10)).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].path, "a.txt");
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn semantic_search_backend_is_swappable() {
        struct Noop;
        impl SemanticSearchBackend for Noop {
            fn name(&self) -> &'static str {
                "noop"
            }
            fn search(
                &self,
                _docs: &[SemDoc<'_>],
                _query: &str,
                _cap: usize,
            ) -> Vec<IndexSearchHit> {
                Vec::new()
            }
        }
        let ws = std::env::temp_dir().join(format!("ai-editor-idx-semswap-{}", std::process::id()));
        std::fs::create_dir_all(&ws).unwrap();
        std::fs::write(ws.join("a.txt"), "apple red\n").unwrap();
        let idx = SearchIndex::default();
        assert_eq!(
            idx.semantic_search(&ws, "apple", Some(10)).unwrap().len(),
            1
        );
        idx.set_semantic_backend(Box::new(Noop));
        assert!(
            idx.semantic_search(&ws, "apple", Some(10))
                .unwrap()
                .is_empty(),
            "replaced keyword backend no longer matches"
        );
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn update_paths_tracks_edits_and_deletes_incrementally() {
        let ws = std::env::temp_dir().join(format!("ai-editor-idx-up-{}", std::process::id()));
        std::fs::create_dir_all(&ws).unwrap();
        std::fs::write(ws.join("a.txt"), "apple\n").unwrap();
        std::fs::write(ws.join("b.txt"), "banana\n").unwrap();
        let idx = SearchIndex::default();
        let st = idx.rebuild(&ws).unwrap();
        assert_eq!(st.indexed_files, 2);
        // No-op when the index covers a different workspace.
        let other =
            std::env::temp_dir().join(format!("ai-editor-idx-other-{}", std::process::id()));
        assert!(idx.update_paths(&other, &["a.txt".to_string()]).is_none());
        // Modify a.txt → content updates.
        std::fs::write(ws.join("a.txt"), "apple banana\n").unwrap();
        let st = idx.update_paths(&ws, &["a.txt".to_string()]).unwrap();
        assert_eq!(st.indexed_files, 2);
        assert_eq!(idx.search(&ws, "apple", None).unwrap().len(), 1);
        // Delete b.txt → removed from the index.
        std::fs::remove_file(ws.join("b.txt")).unwrap();
        let st = idx.update_paths(&ws, &["b.txt".to_string()]).unwrap();
        assert_eq!(st.indexed_files, 1);
        let hits = idx.search(&ws, "banana", None).unwrap();
        assert!(!hits.iter().any(|h| h.path == "b.txt"));
        // Hidden paths are ignored.
        std::fs::write(ws.join(".hidden.txt"), "secret apple\n").unwrap();
        assert!(idx
            .update_paths(&ws, &[".hidden.txt".to_string()])
            .is_none());
        let _ = std::fs::remove_dir_all(&ws);
        let _ = std::fs::remove_dir_all(&other);
    }
}
