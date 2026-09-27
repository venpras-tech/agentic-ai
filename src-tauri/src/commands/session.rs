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
                let updated = modified_ms(&path);
                let title = chat_title(&path).unwrap_or_else(|| "Default chat".to_string());
                if let Some(p) = projects.iter_mut().find(|p| p.key == key) {
                    p.chats.push(SessionChatInfo {
                        id: String::new(),
                        title,
                        updated_at_ms: updated,
                        turns: count_lines(&path),
                        parent_project: None,
                        parent_chat_id: None,
                        fork_turn_id: None,
                        forked_at_ms: None,
                    });
                    p.last_active_ms = p.last_active_ms.max(updated);
                } else {
                    projects.push(SessionProjectInfo {
                        key,
                        name,
                        last_active_ms: updated,
                        chats: vec![SessionChatInfo {
                            id: String::new(),
                            title,
                            updated_at_ms: updated,
                            turns: count_lines(&path),
                            parent_project: None,
                            parent_chat_id: None,
                            fork_turn_id: None,
                            forked_at_ms: None,
                        }],
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
                        if !kp.is_file()
                            || kp.extension().and_then(|e| e.to_str()) != Some("jsonl")
                        {
                            continue;
                        }
                        let Some(id) = kp.file_stem().and_then(|s| s.to_str()) else {
                            continue;
                        };
                        let id = id.to_string();
                        let updated = modified_ms(&kp);
                        last = last.max(updated);
                        let meta = read_chat_meta(&dir, &key, &id);
                        chats.push(SessionChatInfo {
                            title: chat_title(&kp).unwrap_or_else(|| "Empty chat".to_string()),
                            id,
                            updated_at_ms: updated,
                            turns: count_lines(&kp),
                            parent_project: meta.as_ref().and_then(|m| m.parent_project.clone()),
                            parent_chat_id: meta.as_ref().and_then(|m| m.parent_chat_id.clone()),
                            fork_turn_id: meta.as_ref().and_then(|m| m.fork_turn_id.clone()),
                            forked_at_ms: meta.as_ref().and_then(|m| m.forked_at_ms),
                        });
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
        tokio::fs::remove_file(&file).await.map_err(|e| e.to_string())?;
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

/// True if any JSONL line in `text` already carries the given client turn id.
/// Used by [`session_append`] to make replays idempotent: a retried write for a
/// turn the backend already recorded is a no-op instead of a duplicate.
fn log_has_turn_id(text: &str, turn_id: &str) -> bool {
    for line in text.lines() {
        if let Ok(v) = serde_json::from_str::<Value>(line) {
            if v.get("turnId").and_then(|x| x.as_str()) == Some(turn_id) {
                return true;
            }
        }
    }
    false
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
    let turn_id = if record.turn_id.is_empty() {
        None
    } else {
        Some(record.turn_id.clone())
    };
    let dir = app_data_dir(&app).join("sessions");
    tokio::fs::create_dir_all(&dir).await.map_err(|e| e.to_string())?;
    let safe = session_key(&project);
    remember_project_name(&dir, &safe, &project).await;
    let file = session_file(&dir, &project, chat_id.as_deref());
    if let Some(parent) = file.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| e.to_string())?;
    }
    if let Some(tid) = &turn_id {
        if let Ok(text) = tokio::fs::read_to_string(&file).await {
            if log_has_turn_id(&text, tid) {
                return Ok(());
            }
        }
    }
    let mut line = serde_json::to_string(&record).map_err(|e| e.to_string())?;
    line.push('\n');
    use tokio::io::AsyncWriteExt;
    let mut f = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&file)
        .await
        .map_err(|e| e.to_string())?;
    f.write_all(line.as_bytes()).await.map_err(|e| e.to_string())
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
}

fn chat_meta_path(dir: &Path, project: &str, chat_id: &str) -> PathBuf {
    session_file(dir, project, Some(chat_id)).with_extension("meta.json")
}

fn read_chat_meta(dir: &Path, project: &str, chat_id: &str) -> Option<ChatMeta> {
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
    let last_tid = if last_tid.is_empty() { None } else { Some(last_tid) };

    let target_file = session_file(dir, project, Some(target_chat));
    if let Some(parent) = target_file.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let existing = std::fs::read_to_string(&target_file).unwrap_or_default();
    let mut out = String::new();
    let mut written = 0usize;
    for r in &records[..end] {
        if !r.turn_id.is_empty() && log_has_turn_id(&existing, &r.turn_id) {
            continue;
        }
        let mut line = serde_json::to_string(r).map_err(|e| e.to_string())?;
        line.push('\n');
        out.push_str(&line);
        written += 1;
    }
    if written > 0 {
        std::fs::write(&target_file, out).map_err(|e| e.to_string())?;
    }
    let meta = ChatMeta {
        parent_project: Some(project.to_string()),
        parent_chat_id: Some(src_chat.to_string()),
        fork_turn_id: last_tid.clone(),
        forked_at_ms: Some(now_ms()),
    };
    std::fs::write(
        chat_meta_path(dir, project, target_chat),
        serde_json::to_string(&meta).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
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
        fork_session_log(&dir, &project, &src, &target_for_io, fork_turn_id.as_deref())
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
        chat_key, chat_meta_path, fork_session_log, log_has_turn_id, read_chat_meta,
        session_file, session_key,
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
    fn turn_id_dedup_detects_replays() {
        let log = concat!(
            r#"{"role":"user","content":"hi","turnId":"turn-1"}"#,
            "\n",
            r#"{"role":"assistant","content":"hello","turnId":"turn-1"}"#,
            "\n",
            r#"{"role":"user","content":"second"}"#,
            "\n",
        );
        assert!(log_has_turn_id(log, "turn-1"));
        assert!(!log_has_turn_id(log, "turn-2"));
        // Legacy records without a turn id never collide.
        let legacy = r#"{"role":"user","content":"hi"}\n"#;
        assert!(!log_has_turn_id(legacy, "turn-1"));
        // No panic on a broken/unparseable line.
        assert!(!log_has_turn_id("{{{not json\n{\"role\":\"user\"}\n", "turn-1"));
    }

    #[test]
    fn fork_copies_whole_log_and_records_lineage() {
        let dir = temp_dir("fork-whole");
        write_log(
            &dir,
            "chat-a",
            &[rec("u1", "user", "hi"), rec("a1", "assistant", "hello"), rec("u2", "user", "bye")],
        );
        let (copied, fork_turn) =
            fork_session_log(&dir, "D:\\ai", "chat-a", "chat-b", None).unwrap();
        assert_eq!(copied, 3);
        assert_eq!(fork_turn.as_deref(), Some("u2"));
        let forked = read_log(&dir, "chat-b");
        assert_eq!(forked.len(), 3);
        assert_eq!(forked[0].turn_id, "u1");
        assert_eq!(forked[2].turn_id, "u2");
        let meta = read_chat_meta(&dir, "D:\\ai", "chat-b").unwrap();
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
        assert!(chat_meta_path(&dir, "D:\\ai", "chat-b").exists());
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
        let recs: Vec<super::SessionRecord> =
            (0..25).map(|i| rec(&format!("u{i}"), "user", "x")).collect();
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