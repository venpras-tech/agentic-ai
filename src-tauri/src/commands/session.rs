//! Session-history persistence: JSONL chat logs under `{app_data}/sessions`,
//! project/chat browsing, append/load/fork/tail and follow-in logs. Moved out
//! of `main.rs` during the command-layer split; the legacy flat-file layout
//! and dedup semantics are preserved byte-for-byte.

use std::path::{Path, PathBuf};

use serde_json::Value;
use tauri::AppHandle;

use crate::app_data_dir;

/// Sanitize a project string into a filesystem-safe key. This is the legacy
/// scheme (separators become underscores) and must stay byte-stable: existing
/// `sessions/<key>.jsonl` logs keep working across upgrades.
fn session_key(project: &str) -> String {
    project
        .replace(['/', '\\', ':'], "_")
        .trim_matches('_')
        .to_string()
}

/// Sanitize a chat id into a file-name stem (alphanumerics, `-`, `_` only, so
/// named-chat files can never escape the sessions directory).
fn chat_key(chat_id: &str) -> String {
    let cleaned: String = chat_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = cleaned.trim_matches('_').to_string();
    if trimmed.is_empty() {
        "chat".to_string()
    } else {
        trimmed.chars().take(64).collect()
    }
}

/// Resolve the JSONL log file for a `{project, chat}` pair.
///
/// The default chat keeps the legacy flat layout (`<key>.jsonl`) so pre-BN-11
/// history is untouched; named chats live in a per-project subdirectory
/// (`<key>/<chat>.jsonl`). The subdirectory also disambiguates cleanly — the
/// flat key itself may contain `__` runs (`D:\ai` → `D__ai`), so a flat
/// `<key>__<chat>.jsonl` naming would be ambiguous.
fn session_file(dir: &Path, project: &str, chat_id: Option<&str>) -> PathBuf {
    let safe = session_key(project);
    match chat_id.map(str::trim).filter(|c| !c.is_empty()) {
        None => dir.join(format!("{safe}.jsonl")),
        Some(chat) => dir.join(&safe).join(format!("{}.jsonl", chat_key(chat))),
    }
}

/// Remember the original workspace path behind a sanitized key
/// (`sessions/projects.json`) so the projects tree can show real names.
async fn remember_project_name(dir: &Path, key: &str, original: &str) {
    if key == original {
        return;
    }
    let idx_path = dir.join("projects.json");
    let mut map: std::collections::BTreeMap<String, String> = tokio::fs::read_to_string(&idx_path)
        .await
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    if map.get(key).map(String::as_str) != Some(original) {
        map.insert(key.to_string(), original.to_string());
        if let Ok(json) = serde_json::to_string_pretty(&map) {
            let _ = tokio::fs::write(idx_path, json).await;
        }
    }
}

/// Best-effort chat title for the projects tree: the first user message's
/// first line, truncated to 64 chars. Reads at most the first 8 KiB.
fn chat_title(path: &Path) -> Option<String> {
    use std::io::Read;
    let mut f = std::fs::File::open(path).ok()?;
    let mut buf = [0u8; 8192];
    let n = f.read(&mut buf).ok()?;
    for line in String::from_utf8_lossy(&buf[..n]).lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if v.get("role").and_then(Value::as_str) != Some("user") {
            continue;
        }
        let content = v.get("content").and_then(Value::as_str).unwrap_or("");
        let title: String = content
            .lines()
            .next()
            .unwrap_or("")
            .trim()
            .chars()
            .take(64)
            .collect();
        return if title.is_empty() { None } else { Some(title) };
    }
    None
}

/// Build the sidebar row for one chat, preferring the cached meta sidecar.
///
/// `chat_title` and `count_lines` each open and read the transcript, so the
/// pre-index scan parsed every record of every chat on every history-panel
/// open. When the sidecar carries a title and count the transcript is never
/// touched; `modified_ms` still supplies a timestamp for chats written before
/// the sidecar existed. Legacy chats fall back to the transcript reads and are
/// re-cached by their next append.
fn cached_chat_info(
    id: Option<&str>,
    path: &Path,
    meta: Option<&ChatMeta>,
    fallback_title: &str,
) -> SessionChatInfo {
    let updated = meta
        .and_then(|m| m.updated_at_ms)
        .unwrap_or_else(|| modified_ms(path));
    let title = meta
        .and_then(|m| m.title.clone())
        .or_else(|| chat_title(path))
        .unwrap_or_else(|| fallback_title.to_string());
    let turns = meta
        .and_then(|m| m.turns)
        .unwrap_or_else(|| count_lines(path));
    SessionChatInfo {
        id: id.unwrap_or_default().to_string(),
        title,
        updated_at_ms: updated,
        turns,
        parent_project: meta.and_then(|m| m.parent_project.clone()),
        parent_chat_id: meta.and_then(|m| m.parent_chat_id.clone()),
        fork_turn_id: meta.and_then(|m| m.fork_turn_id.clone()),
        forked_at_ms: meta.and_then(|m| m.forked_at_ms),
    }
}

fn modified_ms(path: &Path) -> u64 {
    path.metadata()
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn count_lines(path: &Path) -> usize {
    use std::io::{BufRead, BufReader};
    std::fs::File::open(path)
        .map(|f| BufReader::new(f).lines().count())
        .unwrap_or(0)
}

/// One chat under a project, as shown in the sidebar tree. `id` is empty for
/// the default (legacy) chat.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionChatInfo {
    id: String,
    title: String,
    updated_at_ms: u64,
    turns: usize,
    /// Fork lineage (present when this chat was created by [`session_fork`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    parent_project: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    parent_chat_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    fork_turn_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    forked_at_ms: Option<u64>,
}

/// One project with all of its chats (projects/chats sidebar tree).
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SessionProjectInfo {
    /// Filesystem-safe key; stable across launches.
    key: String,
    /// Original workspace path when known, else the sanitized key. Frontend
    /// calls use this string form (it sanitizes back to `key`).
    name: String,
    last_active_ms: u64,
    chats: Vec<SessionChatInfo>,
}

/// List every known project and its chats, newest activity first.
#[tauri::command]
pub async fn session_projects(app: AppHandle) -> Result<Vec<SessionProjectInfo>, String> {
    let dir = app_data_dir(&app).join("sessions");
    tokio::task::spawn_blocking(move || -> Result<Vec<SessionProjectInfo>, String> {
        if !dir.exists() {
            return Ok(Vec::new());
        }
        let names: std::collections::BTreeMap<String, String> =
            std::fs::read_to_string(dir.join("projects.json"))
                .ok()
                .and_then(|t| serde_json::from_str(&t).ok())
                .unwrap_or_default();
        let mut projects: Vec<SessionProjectInfo> = Vec::new();
        let entries = std::fs::read_dir(&dir).map_err(|e| e.to_string())?;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() && path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
                let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                    continue;
                };
                let key = stem.to_string();
                let name = names.get(&key).cloned().unwrap_or_else(|| key.clone());
                let meta = read_chat_meta(&dir, &key, None);
                let info = cached_chat_info(None, &path, meta.as_ref(), "Default chat");
                let updated = info.updated_at_ms;
                let key = key.clone();
                if let Some(p) = projects.iter_mut().find(|p| p.key == key) {
                    p.chats.push(info);
                    p.last_active_ms = p.last_active_ms.max(updated);
                } else {
                    projects.push(SessionProjectInfo {
                        key,
                        name,
                        last_active_ms: updated,
                        chats: vec![info],
                    });
                }
            } else if path.is_dir() {
                let Some(key) = path.file_name().and_then(|s| s.to_str()) else {
                    continue;
                };
                let key = key.to_string();
                let name = names.get(&key).cloned().unwrap_or_else(|| key.clone());
                let mut chats: Vec<SessionChatInfo> = Vec::new();
                let mut last = 0u64;
                if let Ok(kids) = std::fs::read_dir(&path) {
                    for kid in kids.flatten() {
                        let kp = kid.path();
                        if !kp.is_file() || kp.extension().and_then(|e| e.to_str()) != Some("jsonl")
                        {
                            continue;
                        }
                        let Some(id) = kp.file_stem().and_then(|s| s.to_str()) else {
                            continue;
                        };
                        let id = id.to_string();
                        let meta = read_chat_meta(&dir, &key, Some(&id));
                        let info = cached_chat_info(Some(&id), &kp, meta.as_ref(), "Empty chat");
                        last = last.max(info.updated_at_ms);
                        chats.push(info);
                    }
                }
                match projects.iter_mut().find(|p| p.key == key) {
                    Some(p) => {
                        p.chats.extend(chats);
                        p.last_active_ms = p.last_active_ms.max(last);
                    }
                    None => projects.push(SessionProjectInfo {
                        key,
                        name,
                        last_active_ms: last,
                        chats,
                    }),
                }
            }
        }
        for p in &mut projects {
            p.chats.sort_by_key(|c| std::cmp::Reverse(c.updated_at_ms));
        }
        projects.sort_by_key(|p| std::cmp::Reverse(p.last_active_ms));
        Ok(projects)
    })
    .await
    .map_err(|e| format!("Session scan task panicked: {e}"))?
}

/// Delete a named chat's log. The default (legacy) chat cannot be deleted.
#[tauri::command]
pub async fn session_delete_chat(
    app: AppHandle,
    project: String,
    chat_id: String,
) -> Result<(), String> {
    let chat = chat_id.trim();
    if chat.is_empty() {
        return Err("The default chat cannot be deleted".into());
    }
    let dir = app_data_dir(&app).join("sessions");
    let file = session_file(&dir, &project, Some(chat));
    if file.exists() {
        tokio::fs::remove_file(&file)
            .await
            .map_err(|e| e.to_string())?;
    }
    if let Some(parent) = file.parent() {
        if parent != dir && parent.is_dir() {
            let _ = tokio::fs::remove_dir(parent).await; // only removes when empty
        }
    }
    Ok(())
}

/// One conversation record in a project chat's JSONL session log (camelCase
/// over the wire, `{app_data}/sessions/…`). Only the fields the backend itself
/// touches are carved out; everything else is preserved verbatim in `rest`, so
/// an append→load round trip never drops client-side payload (the log is
/// append-only and replay-safe by `turnId`).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SessionRecord {
    /// Client-generated turn UUID; makes [`session_append`] idempotent (a
    /// replay carrying the same `turnId` is a no-op instead of a duplicate).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    turn_id: String,
    /// All other record fields (role, content, ts, done, kind, …), preserved
    /// byte-for-byte across the model boundary.
    #[serde(flatten)]
    rest: std::collections::HashMap<String, Value>,
}

/// First line of a user message, capped at 64 chars — the chat's display name.
/// Only user messages can name a chat, matching [`chat_title`]'s rule.
fn title_from_record(record: &SessionRecord) -> Option<String> {
    if record.rest.get("role").and_then(Value::as_str) != Some("user") {
        return None;
    }
    record
        .rest
        .get("content")
        .and_then(Value::as_str)
        .map(|content| {
            content
                .lines()
                .next()
                .unwrap_or("")
                .trim()
                .chars()
                .take(64)
                .collect::<String>()
        })
        .filter(|t| !t.is_empty())
}

/// Append one record to a chat, maintaining both sidecars.
///
/// Records carrying a client `turnId` are deduplicated: appending the same turn
/// again is a no-op, so retried / replayed writes can never duplicate a turn.
/// Pure filesystem helper behind [`session_append`] so it is unit-testable
/// without a Tauri app handle.
fn append_record(
    dir: &Path,
    project: &str,
    chat_id: Option<&str>,
    record: &SessionRecord,
) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let file = session_file(dir, project, chat_id);
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }

    let meta = read_chat_meta(dir, project, chat_id);
    let mut ids = read_turn_ids(dir, project, chat_id, meta.as_ref());
    let turn_id = if record.turn_id.is_empty() {
        None
    } else {
        Some(record.turn_id.clone())
    };
    if let Some(tid) = &turn_id {
        if ids.iter().any(|existing| existing == tid) {
            return Ok(());
        }
    }

    let mut line = serde_json::to_string(record).map_err(|e| e.to_string())?;
    line.push('\n');
    // Record first, index second: a crash in between leaves the index one short,
    // which `turn_index_is_stale` detects and repairs on the next write. The
    // reverse order could drop a turn entirely.
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&file)
        .map_err(|e| e.to_string())?;
    {
        use std::io::Write;
        f.write_all(line.as_bytes()).map_err(|e| e.to_string())?;
    }

    let title = meta
        .as_ref()
        .and_then(|m| m.title.clone())
        .or_else(|| title_from_record(record));
    let turns = match meta.as_ref().and_then(|m| m.turns) {
        // Pre-index history has no cached count: `ids` came from a transcript
        // scan, so its length is the true starting total.
        None => ids.len() + 1,
        Some(n) => n + 1,
    };
    if let Some(tid) = &turn_id {
        ids.push(tid.clone());
        let mut index = ids.join("\n");
        index.push('\n');
        std::fs::write(chat_turn_index_path(dir, project, chat_id), index)
            .map_err(|e| e.to_string())?;
    }
    write_chat_meta(
        dir,
        project,
        chat_id,
        &ChatMeta {
            parent_project: meta.as_ref().and_then(|m| m.parent_project.clone()),
            parent_chat_id: meta.as_ref().and_then(|m| m.parent_chat_id.clone()),
            fork_turn_id: meta.as_ref().and_then(|m| m.fork_turn_id.clone()),
            forked_at_ms: meta.as_ref().and_then(|m| m.forked_at_ms),
            title,
            turns: Some(turns),
            updated_at_ms: Some(now_ms()),
            indexed_turns: Some(ids.len()),
        },
    )
}

/// Append one conversation record to the project+chat JSONL session log.
/// Records carrying a client `turnId` are deduplicated: appending the same turn
/// again is a no-op, so retried / replayed writes can never duplicate a turn.
#[tauri::command]
pub async fn session_append(
    app: AppHandle,
    project: String,
    record: SessionRecord,
    chat_id: Option<String>,
) -> Result<(), String> {
    let dir = app_data_dir(&app).join("sessions");
    let append_dir = dir.clone();
    let append_project = project.clone();
    let project_key = session_key(&project);
    tokio::task::spawn_blocking(move || {
        append_record(&append_dir, &append_project, chat_id.as_deref(), &record)
    })
    .await
    .map_err(|e| format!("session append task panicked: {e}"))??;
    remember_project_name(&dir, &project_key, &project).await;
    Ok(())
}

/// Load the full history for one project chat (all JSONL records in order).
#[tauri::command]
pub async fn session_load(
    app: AppHandle,
    project: String,
    chat_id: Option<String>,
) -> Result<Vec<SessionRecord>, String> {
    let dir = app_data_dir(&app).join("sessions");
    let file = session_file(&dir, &project, chat_id.as_deref());
    let Ok(text) = tokio::fs::read_to_string(&file).await else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for line in text.lines() {
        if let Ok(v) = serde_json::from_str::<SessionRecord>(line) {
            out.push(v);
        }
    }
    Ok(out)
}

/// Sidecar lineage file for a forked chat: `sessions/<key>/<chat>.meta.json`.
/// Lives beside the chat's JSONL so the projects scan can surface branch info
/// without touching the append-only transcript format (replays stay stable).
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChatMeta {
    /// Original workspace name of the parent (not its sanitized key).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    parent_project: Option<String>,
    /// `None` = the parent was the legacy default chat.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    parent_chat_id: Option<String>,
    /// Last record copied into the fork (the fork point). Absent when the
    /// source had no `turnId` on that record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    fork_turn_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    forked_at_ms: Option<u64>,
    /// Chat title, derived from the first user message and cached here so
    /// listing never has to open the transcript.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    title: Option<String>,
    /// Record count in the `.jsonl`, cached for the same reason.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    turns: Option<usize>,
    /// Last activity, cached so sorting does not stat every transcript.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    updated_at_ms: Option<u64>,
    /// How many turn ids the `.turns` sidecar holds. A mismatch against the
    /// sidecar's real line count means a crash landed between the two writes,
    /// so the index is rebuilt from the transcript instead of trusted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    indexed_turns: Option<usize>,
}

fn chat_meta_path(dir: &Path, project: &str, chat_id: Option<&str>) -> PathBuf {
    session_file(dir, project, chat_id).with_extension("meta.json")
}

/// Append-only log of the turn ids a chat has already recorded, one per line.
///
/// This is what makes [`session_append`] idempotent without re-reading the
/// transcript: the old path read and JSON-parsed every record on every append,
/// which is O(n^2) over a chat's lifetime. The `.jsonl` stays the source of
/// truth — this file is a rebuildable cache, and [`turn_index_is_stale`] detects
/// the case where it disagrees with the log and forces a rebuild.
fn chat_turn_index_path(dir: &Path, project: &str, chat_id: Option<&str>) -> PathBuf {
    session_file(dir, project, chat_id).with_extension("turns")
}

/// Does the turn index need rebuilding from the transcript?
///
/// The append writes the record first and the id second, so a crash in between
/// leaves the index one behind — a replay would then append a duplicate. We
/// detect that by comparing the index's line count against `indexed_turns`
/// recorded in the chat meta, and rebuild rather than risk a duplicate turn.
fn turn_index_is_stale(index_lines: usize, meta: Option<&ChatMeta>) -> bool {
    match meta.and_then(|m| m.indexed_turns) {
        Some(expected) => index_lines != expected,
        // No meta yet: either pre-index history (index absent or stale) or a
        // genuinely fresh chat. Rebuilding costs one scan of a missing or tiny
        // file in the fresh case, and the meta written afterwards pins the
        // count so this stops happening.
        None => index_lines == 0,
    }
}

/// Read the recorded turn ids, rebuilding from the transcript when the index
/// cannot be trusted.
///
/// Worst case (a missing or stale index) this parses the log once, exactly like
/// the pre-index code path, so correctness never depends on the cache.
fn read_turn_ids(
    dir: &Path,
    project: &str,
    chat_id: Option<&str>,
    meta: Option<&ChatMeta>,
) -> Vec<String> {
    let index_path = chat_turn_index_path(dir, project, chat_id);
    let mut ids: Vec<String> = std::fs::read_to_string(&index_path)
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect();
    if turn_index_is_stale(ids.len(), meta) {
        ids = std::fs::read_to_string(session_file(dir, project, chat_id))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter_map(|v| v.get("turnId").and_then(Value::as_str).map(str::to_string))
            .collect();
    }
    ids
}

/// Refresh a chat's meta sidecar: the per-session index record that keeps
/// listing off the transcript.
///
/// Stores the title, turn count and last activity so the projects scan never
/// has to open a `.jsonl`. Kept beside the fork lineage fields, which already
/// share this file, so a chat has exactly one sidecar.
fn write_chat_meta(
    dir: &Path,
    project: &str,
    chat_id: Option<&str>,
    meta: &ChatMeta,
) -> Result<(), String> {
    let path = chat_meta_path(dir, project, chat_id);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(
        &path,
        serde_json::to_string(meta).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())
}

fn read_chat_meta(dir: &Path, project: &str, chat_id: Option<&str>) -> Option<ChatMeta> {
    std::fs::read_to_string(chat_meta_path(dir, project, chat_id))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Copy a prefix of one named chat's JSONL into a new named chat and record the
/// fork lineage in a `.meta.json` sidecar. `fork_turn_id` selects the last
/// record to copy; when it lands on a user turn, its assistant reply rides
/// along so the fork always starts on a clean turn boundary. Copying the whole
/// log (no `fork_turn_id`) acts as a "clone" — still lineage-tracked. Records
/// already present in the target (by `turnId`) are never duplicated. Pure
/// filesystem helper behind [`session_fork`] so it is unit-testable.
fn fork_session_log(
    dir: &Path,
    project: &str,
    src_chat: &str,
    target_chat: &str,
    fork_turn_id: Option<&str>,
) -> Result<(usize, Option<String>), String> {
    let src_file = session_file(dir, project, Some(src_chat));
    let records: Vec<SessionRecord> = std::fs::read_to_string(&src_file)
        .map_err(|e| format!("cannot read source chat: {e}"))?
        .lines()
        .filter_map(|l| serde_json::from_str::<SessionRecord>(l).ok())
        .collect();
    if records.is_empty() {
        return Err("source chat has no records".into());
    }
    let mut end = records.len();
    if let Some(tid) = fork_turn_id.filter(|t| !t.is_empty()) {
        let idx = records
            .iter()
            .position(|r| r.turn_id == tid)
            .ok_or_else(|| format!("fork point {tid} not found in source chat"))?;
        end = idx + 1;
        // Records interleave user/assistant per turn; pull the reply too.
        if end < records.len() {
            let last = records[end - 1].rest.get("role").and_then(Value::as_str);
            let next = records[end].rest.get("role").and_then(Value::as_str);
            if last == Some("user") && next.is_some() && next != Some("user") {
                end += 1;
            }
        }
    }
    let last_tid = records[end - 1].turn_id.clone();
    let last_tid = if last_tid.is_empty() {
        None
    } else {
        Some(last_tid)
    };

    let target_file = session_file(dir, project, Some(target_chat));
    if let Some(parent) = target_file.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    // Build the seen-set once: the previous code re-scanned the target log for
    // every copied record, making a fork of a long chat O(n^2) as well.
    let existing_ids: std::collections::HashSet<String> =
        read_turn_ids(dir, project, Some(target_chat), None)
            .into_iter()
            .collect();
    let mut out = String::new();
    let mut written = 0usize;
    let mut copied_ids: Vec<String> = Vec::new();
    for r in &records[..end] {
        if !r.turn_id.is_empty() && existing_ids.contains(&r.turn_id) {
            continue;
        }
        let mut line = serde_json::to_string(r).map_err(|e| e.to_string())?;
        line.push('\n');
        out.push_str(&line);
        if !r.turn_id.is_empty() {
            copied_ids.push(r.turn_id.clone());
        }
        written += 1;
    }
    if written > 0 {
        // Truncate, then append: the target may already hold records that the
        // dedupe skipped, and a plain append would interleave out of order.
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&target_file)
            .map_err(|e| e.to_string())?;
        use std::io::Write;
        f.write_all(out.as_bytes()).map_err(|e| e.to_string())?;
        drop(f);
    }

    // The fork inherits the target's real turn count and ids so its sidecar
    // starts consistent and later appends stay O(1).
    let target_meta = read_chat_meta(dir, project, Some(target_chat));
    let mut all_ids = read_turn_ids(dir, project, Some(target_chat), target_meta.as_ref());
    for id in &copied_ids {
        if !all_ids.contains(id) {
            all_ids.push(id.clone());
        }
    }
    if !all_ids.is_empty() {
        let mut index = all_ids.join("\n");
        index.push('\n');
        std::fs::write(chat_turn_index_path(dir, project, Some(target_chat)), index)
            .map_err(|e| e.to_string())?;
    }
    let total_turns = if let Some(n) = target_meta.as_ref().and_then(|m| m.turns) {
        n + written
    } else {
        std::fs::read_to_string(&target_file)
            .unwrap_or_default()
            .lines()
            .count()
    };
    let title = target_meta
        .as_ref()
        .and_then(|m| m.title.clone())
        .or_else(|| {
            records[..end]
                .iter()
                .find(|r| r.rest.get("role").and_then(Value::as_str) == Some("user"))
                .and_then(|r| r.rest.get("content").and_then(Value::as_str))
                .map(|c| {
                    c.lines()
                        .next()
                        .unwrap_or("")
                        .trim()
                        .chars()
                        .take(64)
                        .collect::<String>()
                })
        })
        .filter(|t| !t.is_empty());
    write_chat_meta(
        dir,
        project,
        Some(target_chat),
        &ChatMeta {
            parent_project: Some(project.to_string()),
            parent_chat_id: Some(src_chat.to_string()),
            fork_turn_id: last_tid.clone(),
            forked_at_ms: Some(now_ms()),
            title,
            turns: Some(total_turns),
            updated_at_ms: Some(modified_ms(&target_file)),
            indexed_turns: Some(all_ids.len()),
        },
    )?;
    Ok((written, last_tid))
}

/// Return value of [`session_fork`].
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ForkInfo {
    target_chat_id: String,
    copied: usize,
    fork_turn_id: Option<String>,
}

/// Branch a named chat: copy its history (whole log, or up to a `fork_turn_id`)
/// into a brand-new named chat and keep a lineage marker beside the new log, so
/// the UI can show session ancestry the way Claude/Zed thread forks do.
#[tauri::command]
pub async fn session_fork(
    app: AppHandle,
    project: String,
    chat_id: String,
    fork_turn_id: Option<String>,
    target_chat_id: String,
) -> Result<ForkInfo, String> {
    let src = chat_id.trim().to_string();
    let target = target_chat_id.trim().to_string();
    if src.is_empty() {
        return Err("Only a named chat can be forked (the default chat is the base log)".into());
    }
    if target.is_empty() {
        return Err("A named target chat id is required".into());
    }
    if chat_key(&target) != target {
        return Err("target_chat_id may contain only letters, digits, '-' and '_'".into());
    }
    if target == src {
        return Err("target chat must differ from the source chat".into());
    }
    let dir = app_data_dir(&app).join("sessions");
    remember_project_name(&dir, &session_key(&project), &project).await;
    let target_for_io = target.clone();
    let (copied, fork_turn) = tokio::task::spawn_blocking(move || {
        fork_session_log(
            &dir,
            &project,
            &src,
            &target_for_io,
            fork_turn_id.as_deref(),
        )
    })
    .await
    .map_err(|e| format!("fork task panicked: {e}"))??;
    Ok(ForkInfo {
        target_chat_id: target,
        copied,
        fork_turn_id: fork_turn,
    })
}

/// Read only the last `limit` records of a chat (default 20, capped at 200).
/// Stateless, so clients can poll it for a lightweight live "watch" of a
/// session's transcript tail (foreground or background) without a subscription
/// transport.
#[tauri::command]
pub async fn session_tail(
    app: AppHandle,
    project: String,
    chat_id: Option<String>,
    limit: Option<usize>,
) -> Result<Vec<SessionRecord>, String> {
    let max = limit.unwrap_or(20).clamp(1, 200);
    let mut records = session_load(app, project, chat_id).await?;
    let start = records.len().saturating_sub(max);
    records.drain(..start);
    Ok(records)
}

#[cfg(test)]
mod tests {
    use super::{
        append_record, cached_chat_info, chat_key, chat_meta_path, chat_turn_index_path,
        fork_session_log, read_chat_meta, read_turn_ids, session_file, session_key,
        turn_index_is_stale, write_chat_meta,
    };

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ai-editor-session-tests-{tag}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn rec(turn_id: &str, role: &str, content: &str) -> super::SessionRecord {
        super::SessionRecord {
            turn_id: turn_id.to_string(),
            rest: [
                ("role".to_string(), serde_json::json!(role)),
                ("content".to_string(), serde_json::json!(content)),
            ]
            .into_iter()
            .collect(),
        }
    }

    fn write_log(dir: &std::path::Path, chat: &str, recs: &[super::SessionRecord]) {
        let file = session_file(dir, "D:\\ai", Some(chat));
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        let mut text = String::new();
        for r in recs {
            text.push_str(&serde_json::to_string(r).unwrap());
            text.push('\n');
        }
        std::fs::write(file, text).unwrap();
    }

    fn read_log(dir: &std::path::Path, chat: &str) -> Vec<super::SessionRecord> {
        std::fs::read_to_string(session_file(dir, "D:\\ai", Some(chat)))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str::<super::SessionRecord>(l).ok())
            .collect()
    }

    #[test]
    fn session_key_matches_legacy_sanitization() {
        assert_eq!(session_key("D:\\ai"), "D__ai");
        assert_eq!(session_key("/home/user/proj"), "home_user_proj");
        assert_eq!(session_key("default"), "default");
        // Stable across calls (idempotent on its own output).
        assert_eq!(
            session_key(&session_key("C:\\x\\y")),
            session_key("C:\\x\\y")
        );
    }

    #[test]
    fn chat_key_is_filename_safe() {
        assert_eq!(chat_key("chat-ab12"), "chat-ab12");
        assert_eq!(chat_key("a/b\\c:d"), "a_b_c_d");
        assert_eq!(chat_key("../../../etc"), "etc");
        // No traversal can survive sanitization.
        let k = chat_key("..\\..\\evil");
        assert!(!k.contains('\\') && !k.contains('/'));
        assert_eq!(chat_key("   "), "chat");
    }

    #[test]
    fn default_chat_keeps_legacy_flat_file() {
        let dir = std::path::Path::new("S");
        assert_eq!(session_file(dir, "D:\\ai", None), dir.join("D__ai.jsonl"));
        assert_eq!(
            session_file(dir, "D:\\ai", Some("")),
            dir.join("D__ai.jsonl")
        );
        assert_eq!(
            session_file(dir, "D:\\ai", Some("chat-1")),
            dir.join("D__ai").join("chat-1.jsonl")
        );
    }

    #[test]
    fn the_string_default_is_not_the_default_chat() {
        // Regression guard. The frontend lists the base chat with an empty id
        // and renders it as the sentinel `"default"`. `session_projects` also
        // reports an empty `id` for the flat file. If that sentinel ever reached
        // `session_file` as `Some("default")` it would address
        // `sessions/<key>/default.jsonl`, which nothing writes — history reads
        // returned empty and new turns went to a phantom file. The UI now
        // collapses it back to `null`; this asserts the two layouts stay
        // distinct so that collapse is the only correct mapping.
        let dir = std::path::Path::new("S");
        let flat = session_file(dir, "D:\\ai", None);
        let sentinel = session_file(dir, "D:\\ai", Some("default"));
        assert_ne!(flat, sentinel);
        assert!(flat.parent() == Some(dir), "base chat must stay flat");
        assert!(
            sentinel.parent() != Some(dir),
            "`default` must not collide with the base chat's layout"
        );
    }

    #[test]
    fn append_dedupes_replayed_turn_ids() {
        let dir = temp_dir("append-dedup");
        append_record(&dir, "D:\\ai", Some("chat-a"), &rec("t1", "user", "hi")).unwrap();
        append_record(
            &dir,
            "D:\\ai",
            Some("chat-a"),
            // A turn's two halves share a base id but differ on the wire, so
            // the reply is never mistaken for a replay of the question.
            &rec("t1-assistant", "assistant", "hello"),
        )
        .unwrap();
        // Same turn again: the replay is dropped, not duplicated.
        append_record(&dir, "D:\\ai", Some("chat-a"), &rec("t1", "user", "hi")).unwrap();
        let log = read_log(&dir, "chat-a");
        assert_eq!(log.len(), 2);
        assert_eq!(log[0].rest["content"], serde_json::json!("hi"));
        assert_eq!(log[1].rest["content"], serde_json::json!("hello"));
        // A distinct turn still appends.
        append_record(&dir, "D:\\ai", Some("chat-a"), &rec("t2", "user", "next")).unwrap();
        assert_eq!(read_log(&dir, "chat-a").len(), 3);
    }

    #[test]
    fn append_keeps_records_without_a_turn_id() {
        let dir = temp_dir("append-no-turn-id");
        let mut r = rec("", "user", "legacy one");
        r.turn_id = String::new();
        append_record(&dir, "D:\\ai", Some("chat-a"), &r).unwrap();
        append_record(&dir, "D:\\ai", Some("chat-a"), &r).unwrap();
        // No turn id means no dedup possible, so both writes land; what matters
        // is that a blank id never poisons the index or the cached count.
        assert_eq!(read_log(&dir, "chat-a").len(), 2);
        let meta = read_chat_meta(&dir, "D:\\ai", Some("chat-a")).unwrap();
        assert_eq!(meta.turns, Some(2));
        assert_eq!(meta.indexed_turns, Some(0));
    }

    #[test]
    fn append_indexes_pre_existing_history() {
        let dir = temp_dir("append-legacy");
        // A transcript written before the index existed: no `.turns`, no meta.
        write_log(
            &dir,
            "chat-a",
            &[
                rec("old-1", "user", "first"),
                rec("old-2", "assistant", "reply"),
                rec("old-3", "user", "second"),
            ],
        );
        append_record(
            &dir,
            "D:\\ai",
            Some("chat-a"),
            &rec("new-1", "user", "third"),
        )
        .unwrap();
        let meta = read_chat_meta(&dir, "D:\\ai", Some("chat-a")).unwrap();
        assert_eq!(meta.turns, Some(4), "legacy count must not reset to 1");
        assert_eq!(meta.indexed_turns, Some(4));
        // And the legacy ids are now indexed, so replaying one is still caught.
        append_record(
            &dir,
            "D:\\ai",
            Some("chat-a"),
            &rec("old-2", "assistant", "reply"),
        )
        .unwrap();
        assert_eq!(read_log(&dir, "chat-a").len(), 4);
    }

    #[test]
    fn stale_turn_index_is_rebuilt_not_trusted() {
        let dir = temp_dir("append-stale-index");
        append_record(&dir, "D:\\ai", Some("chat-a"), &rec("t1", "user", "hi")).unwrap();
        // Simulate a crash between the transcript write and the index write:
        // the log holds a turn the index does not.
        append_record(&dir, "D:\\ai", Some("chat-a"), &rec("t2", "user", "two")).unwrap();
        std::fs::write(chat_turn_index_path(&dir, "D:\\ai", Some("chat-a")), "t1\n").unwrap();

        let meta = read_chat_meta(&dir, "D:\\ai", Some("chat-a"));
        let ids = read_turn_ids(&dir, "D:\\ai", Some("chat-a"), meta.as_ref());
        assert_eq!(ids, vec!["t1".to_string(), "t2".to_string()]);

        // The replay of the lost turn is caught instead of duplicating it.
        append_record(&dir, "D:\\ai", Some("chat-a"), &rec("t2", "user", "two")).unwrap();
        assert_eq!(read_log(&dir, "chat-a").len(), 2);
    }

    #[test]
    fn stale_detection_only_fires_on_a_mismatch() {
        let meta = super::ChatMeta {
            indexed_turns: Some(3),
            ..Default::default()
        };
        assert!(!turn_index_is_stale(3, Some(&meta)));
        assert!(turn_index_is_stale(2, Some(&meta)));
        // Without meta an empty index means pre-index history and must rebuild; a
        // populated one is trusted.
        assert!(turn_index_is_stale(0, None));
        assert!(!turn_index_is_stale(5, None));
    }

    #[test]
    fn append_caches_title_and_count_for_listing() {
        let dir = temp_dir("append-meta");
        append_record(
            &dir,
            "D:\\ai",
            Some("chat-a"),
            &rec("t1", "user", "Explain the borrow checker"),
        )
        .unwrap();
        append_record(
            &dir,
            "D:\\ai",
            Some("chat-a"),
            &rec("t1-assistant", "assistant", "Sure, here's how"),
        )
        .unwrap();
        let meta = read_chat_meta(&dir, "D:\\ai", Some("chat-a")).unwrap();
        assert_eq!(meta.title.as_deref(), Some("Explain the borrow checker"));
        assert_eq!(meta.turns, Some(2));
        assert!(meta.updated_at_ms.unwrap() > 0);

        // Listing reads the sidecar, not the transcript.
        let file = session_file(&dir, "D:\\ai", Some("chat-a"));
        let info = cached_chat_info(Some("chat-a"), &file, Some(&meta), "Empty chat");
        assert_eq!(info.title, "Explain the borrow checker");
        assert_eq!(info.turns, 2);

        // A later assistant turn must not steal the title from the first
        // user message.
        append_record(
            &dir,
            "D:\\ai",
            Some("chat-a"),
            &rec("t2", "assistant", "And this too"),
        )
        .unwrap();
        let meta2 = read_chat_meta(&dir, "D:\\ai", Some("chat-a")).unwrap();
        assert_eq!(meta2.title.as_deref(), Some("Explain the borrow checker"));
        assert_eq!(meta2.turns, Some(3));
    }

    #[test]
    fn listing_falls_back_to_the_transcript_without_a_sidecar() {
        let dir = temp_dir("list-legacy");
        write_log(
            &dir,
            "chat-a",
            &[
                rec("t1", "user", "legacy title"),
                rec("t1", "assistant", "reply"),
            ],
        );
        let file = session_file(&dir, "D:\\ai", Some("chat-a"));
        let info = cached_chat_info(Some("chat-a"), &file, None, "Empty chat");
        assert_eq!(info.title, "legacy title");
        assert_eq!(info.turns, 2);
        assert!(info.updated_at_ms > 0);
        assert_eq!(info.id, "chat-a");
    }

    #[test]
    fn fork_seeds_the_index_and_cache() {
        let dir = temp_dir("fork-index");
        write_log(
            &dir,
            "chat-a",
            &[
                rec("t1", "user", "parent question"),
                rec("t1", "assistant", "parent answer"),
            ],
        );
        fork_session_log(&dir, "D:\\ai", "chat-a", "chat-b", None).unwrap();

        let meta = read_chat_meta(&dir, "D:\\ai", Some("chat-b")).unwrap();
        assert_eq!(meta.turns, Some(2));
        assert_eq!(meta.indexed_turns, Some(2));
        assert_eq!(meta.title.as_deref(), Some("parent question"));
        assert_eq!(meta.parent_chat_id.as_deref(), Some("chat-a"));
        assert_eq!(
            read_turn_ids(&dir, "D:\\ai", Some("chat-b"), Some(&meta)),
            vec!["t1".to_string(), "t1".to_string()]
        );

        // Appending to the fork dedupes against the copied ids.
        append_record(
            &dir,
            "D:\\ai",
            Some("chat-b"),
            &rec("t1", "user", "parent question"),
        )
        .unwrap();
        assert_eq!(read_log(&dir, "chat-b").len(), 2);
    }

    #[test]
    fn fork_preserves_existing_target_records() {
        let dir = temp_dir("fork-existing");
        write_log(
            &dir,
            "chat-a",
            &[rec("t1", "user", "q"), rec("t1", "assistant", "a")],
        );
        write_log(&dir, "chat-b", &[rec("keep", "user", "already here")]);
        let (copied, _) = fork_session_log(&dir, "D:\\ai", "chat-a", "chat-b", None).unwrap();
        assert_eq!(copied, 2);
        let ids: Vec<String> = read_log(&dir, "chat-b")
            .into_iter()
            .map(|r| r.turn_id)
            .collect();
        assert_eq!(
            ids,
            vec!["keep".to_string(), "t1".to_string(), "t1".to_string()],
            "the pre-existing record must stay first and un-duplicated"
        );
    }

    #[test]
    fn chat_meta_sidecar_survives_lineage_free_writes() {
        let dir = temp_dir("meta-merge");
        write_chat_meta(
            &dir,
            "D:\\ai",
            Some("chat-a"),
            &super::ChatMeta {
                parent_project: Some("D:\\ai".into()),
                parent_chat_id: Some("chat-parent".into()),
                fork_turn_id: Some("pf".into()),
                forked_at_ms: Some(42),
                title: Some("kept".into()),
                turns: Some(7),
                updated_at_ms: Some(1),
                indexed_turns: Some(0),
            },
        )
        .unwrap();
        append_record(&dir, "D:\\ai", Some("chat-a"), &rec("t8", "user", "new")).unwrap();
        let meta = read_chat_meta(&dir, "D:\\ai", Some("chat-a")).unwrap();
        // Lineage and cached title survive an ordinary append.
        assert_eq!(meta.parent_chat_id.as_deref(), Some("chat-parent"));
        assert_eq!(meta.fork_turn_id.as_deref(), Some("pf"));
        assert_eq!(meta.title.as_deref(), Some("kept"));
        assert_eq!(meta.turns, Some(8));
    }

    #[test]
    fn fork_copies_whole_log_and_records_lineage() {
        let dir = temp_dir("fork-whole");
        write_log(
            &dir,
            "chat-a",
            &[
                rec("u1", "user", "hi"),
                rec("a1", "assistant", "hello"),
                rec("u2", "user", "bye"),
            ],
        );
        let (copied, fork_turn) =
            fork_session_log(&dir, "D:\\ai", "chat-a", "chat-b", None).unwrap();
        assert_eq!(copied, 3);
        assert_eq!(fork_turn.as_deref(), Some("u2"));
        let forked = read_log(&dir, "chat-b");
        assert_eq!(forked.len(), 3);
        assert_eq!(forked[0].turn_id, "u1");
        assert_eq!(forked[2].turn_id, "u2");
        let meta = read_chat_meta(&dir, "D:\\ai", Some("chat-b")).unwrap();
        assert_eq!(meta.parent_chat_id.as_deref(), Some("chat-a"));
        assert_eq!(meta.fork_turn_id.as_deref(), Some("u2"));
        assert!(meta.forked_at_ms.is_some());
        // Forking again must not duplicate already-copied turnIds.
        let (copied, _) = fork_session_log(&dir, "D:\\ai", "chat-a", "chat-b", None).unwrap();
        assert_eq!(copied, 0);
        assert_eq!(read_log(&dir, "chat-b").len(), 3);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fork_slices_at_turn_boundary() {
        let dir = temp_dir("fork-slice");
        // u -> a (reply) -> u2 -> a2
        write_log(
            &dir,
            "chat-a",
            &[
                rec("u1", "user", "first"),
                rec("a1", "assistant", "reply"),
                rec("u2", "user", "second"),
                rec("a2", "assistant", "reply2"),
            ],
        );
        // Fork on the second user turn: its reply must ride along.
        let (copied, fork_turn) =
            fork_session_log(&dir, "D:\\ai", "chat-a", "chat-b", Some("u2")).unwrap();
        assert_eq!(copied, 4);
        assert_eq!(fork_turn.as_deref(), Some("a2"));
        let forked = read_log(&dir, "chat-b");
        assert_eq!(forked.len(), 4);
        assert_eq!(forked[3].turn_id, "a2");
        assert!(chat_meta_path(&dir, "D:\\ai", Some("chat-b")).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fork_on_assistant_turn_stops_at_that_record() {
        let dir = temp_dir("fork-assistant");
        write_log(
            &dir,
            "chat-a",
            &[rec("u1", "user", "q"), rec("a1", "assistant", "a")],
        );
        let (copied, fork_turn) =
            fork_session_log(&dir, "D:\\ai", "chat-a", "chat-b", Some("a1")).unwrap();
        assert_eq!(copied, 2);
        assert_eq!(fork_turn.as_deref(), Some("a1"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fork_missing_point_rejected() {
        let dir = temp_dir("fork-missing");
        write_log(&dir, "chat-a", &[rec("u1", "user", "q")]);
        assert!(fork_session_log(&dir, "D:\\ai", "chat-a", "chat-b", Some("nope")).is_err());
        assert!(fork_session_log(&dir, "D:\\ai", "chat-zz", "chat-b", None).is_err());
        // A chat with no records is not forkable.
        write_log(&dir, "chat-empty", &[]);
        assert!(fork_session_log(&dir, "D:\\ai", "chat-empty", "chat-b", None).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tail_returns_last_n_and_is_bounded() {
        let dir = temp_dir("tail");
        let recs: Vec<super::SessionRecord> = (0..25)
            .map(|i| rec(&format!("u{i}"), "user", "x"))
            .collect();
        write_log(&dir, "chat-a", &recs);
        // Driven through `session_tail`-style math: last `limit`, drained.
        let limit = 20usize;
        let mut all = read_log(&dir, "chat-a");
        let start = all.len().saturating_sub(limit);
        all.drain(..start);
        assert_eq!(all.len(), 20);
        assert_eq!(all[0].turn_id, "u5");
        // A limit of 0 still yields 1 (clamped to the minimum).
        let limit = 0usize.clamp(1, 200);
        let mut all = read_log(&dir, "chat-a");
        let start = all.len().saturating_sub(limit);
        all.drain(..start);
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].turn_id, "u24");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
