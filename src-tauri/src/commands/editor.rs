//! Workspace / file I/O, picker dialogs and the interactive terminal commands
//! (moved out of `main.rs` during the command-layer split).

use std::path::PathBuf;
use std::sync::Arc;

use tauri::{AppHandle, Emitter, State};
use tauri_plugin_dialog::DialogExt;

use crate::agent::skills::KnowledgeState;
use crate::terminal::{TerminalInfo, TerminalSessions};
use crate::{app_config_dir, knowledge_report, sync_knowledge, sync_modes, ContextState};

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FsEntry {
    name: String,
    path: String,
    is_dir: bool,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TextFile {
    path: String,
    content: String,
}

#[tauri::command]
pub async fn pick_workspace_folder(app: AppHandle) -> Result<Option<String>, String> {
    let picked = app.dialog().file().blocking_pick_folder();
    Ok(picked
        .and_then(|p| p.into_path().ok())
        .map(|p| p.to_string_lossy().into_owned()))
}

#[tauri::command]
pub async fn pick_text_file(app: AppHandle) -> Result<Option<String>, String> {
    let picked = app
        .dialog()
        .file()
        .set_title("Open file")
        .add_filter(
            "Text files",
            &[
                "txt", "md", "json", "jsonc", "ts", "tsx", "js", "jsx", "css", "scss", "html",
                "htm", "xml", "yaml", "yml", "toml", "ini", "cfg", "conf", "py", "rs", "go",
                "java", "c", "cpp", "h", "hpp", "cs", "rb", "php", "sh", "bash", "zsh", "fish",
                "ps1", "bat", "cmd", "sql", "graphql", "gql", "proto", "lua", "r", "swift", "kt",
                "vue", "svelte", "astro", "wxml", "wxss",
            ],
        )
        .blocking_pick_file();
    Ok(picked
        .and_then(|p| p.into_path().ok())
        .map(|p| p.to_string_lossy().into_owned()))
}

#[tauri::command]
pub async fn list_directory(
    _app: AppHandle,
    root: String,
    relative: Option<String>,
) -> Result<Vec<FsEntry>, String> {
    let base = PathBuf::from(&root);
    let dir = match relative {
        Some(ref r) if !r.is_empty() => base.join(r),
        _ => base.clone(),
    };
    if !dir.is_dir() {
        return Err(format!("Not a directory: {}", dir.display()));
    }
    let mut entries = Vec::new();
    let mut read_dir = tokio::fs::read_dir(&dir).await.map_err(|e| e.to_string())?;
    while let Ok(Some(entry)) = read_dir.next_entry().await {
        let name = entry.file_name().to_string_lossy().into_owned();
        let is_dir = entry.file_type().await.map_err(|e| e.to_string())?.is_dir();
        let rel = if relative.is_some() {
            dir.join(&name).to_string_lossy().into_owned()
        } else {
            entry.path().to_string_lossy().into_owned()
        };
        entries.push(FsEntry {
            name,
            path: rel,
            is_dir,
        });
    }
    entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then(a.name.cmp(&b.name)));
    Ok(entries)
}

#[tauri::command]
pub async fn read_text_file(_app: AppHandle, path: String) -> Result<TextFile, String> {
    let bytes = tokio::fs::read(&path).await.map_err(|e| e.to_string())?;
    // Accept UTF-8 and common single-byte encodings; never fail on odd files.
    let content = match String::from_utf8(bytes.clone()) {
        Ok(s) => s,
        Err(_) => encoding_rs::WINDOWS_1252.decode(&bytes).0.to_string(),
    };
    Ok(TextFile { path, content })
}

#[tauri::command]
pub async fn write_text_file(path: String, content: String) -> Result<(), String> {
    tokio::fs::write(&path, content)
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Revert a file to its pre-change content (undo an agent edit).
#[tauri::command]
pub async fn revert_file(path: String, before: String) -> Result<(), String> {
    tokio::fs::write(&path, before)
        .await
        .map_err(|e| format!("Failed to revert `{path}`: {e}"))?;
    Ok(())
}

/// Cheap existence check used before reconnecting to the last local model on
/// startup, so the UI can prompt for a new model without a full load attempt.
#[tauri::command]
pub fn path_exists(path: String) -> bool {
    std::path::Path::new(&path).is_file()
}

/// Save-as dialog for untitled buffers; returns the chosen path or `None` if
/// the user cancelled.
#[tauri::command]
pub async fn save_file_as(app: AppHandle, content: String) -> Result<Option<String>, String> {
    let picked = app
        .dialog()
        .file()
        .set_title("Save file as")
        .add_filter("All files", &["*"])
        .blocking_save_file();
    let Some(picked) = picked else {
        return Ok(None);
    };
    let path = picked.into_path().map_err(|e| e.to_string())?;
    tokio::fs::write(&path, content)
        .await
        .map_err(|e| e.to_string())?;
    Ok(Some(path.to_string_lossy().into_owned()))
}

/// Save-as dialog for binary exports (PDF, DOCX, CSV). Accepts base64-encoded
/// bytes and a suggested filename; returns the chosen path or `None` on cancel.
#[tauri::command]
pub async fn save_file_as_bytes(
    app: AppHandle,
    content: String,
    suggested_filename: String,
) -> Result<Option<String>, String> {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&content)
        .map_err(|e| format!("base64 decode failed: {e}"))?;
    let picked = app
        .dialog()
        .file()
        .set_title("Export conversation")
        .set_file_name(&suggested_filename)
        .add_filter("All files", &["*"])
        .blocking_save_file();
    let Some(picked) = picked else {
        return Ok(None);
    };
    let path = picked.into_path().map_err(|e| e.to_string())?;
    tokio::fs::write(&path, bytes)
        .await
        .map_err(|e| e.to_string())?;
    Ok(Some(path.to_string_lossy().into_owned()))
}

// ---------------------------------------------------------------------------
// Interactive terminal (persistent shell sessions)
// ---------------------------------------------------------------------------

/// Spawn a persistent interactive shell and stream output via
/// `agent://terminal-output` events. Returns the terminal id.
#[tauri::command]
pub async fn terminal_spawn(
    app: AppHandle,
    sessions: State<'_, Arc<TerminalSessions>>,
    cwd: Option<String>,
) -> Result<String, String> {
    sessions.spawn(&app, cwd)
}

/// Write one line (plus newline) to a terminal's stdin.
#[tauri::command]
pub async fn terminal_write(
    sessions: State<'_, Arc<TerminalSessions>>,
    id: String,
    line: String,
) -> Result<(), String> {
    sessions.write(&id, &line)
}

/// Terminate a terminal session (kills the process tree).
#[tauri::command]
pub async fn terminal_kill(
    sessions: State<'_, Arc<TerminalSessions>>,
    id: String,
) -> Result<(), String> {
    sessions.kill(&id)
}

/// List live terminal sessions (id + cwd).
#[tauri::command]
pub async fn terminal_list(
    sessions: State<'_, Arc<TerminalSessions>>,
) -> Result<Vec<TerminalInfo>, String> {
    Ok(sessions.list())
}

// ---------------------------------------------------------------------------
// Agent workspace roots
// ---------------------------------------------------------------------------

/// Set the active workspace root that file tools operate on by default.
/// Also scans project skills/rules and syncs them into the context manager.
/// Replaces all existing workspaces (backward-compatible single-root API).
#[tauri::command]
pub async fn agent_set_workspace(
    app: AppHandle,
    state: State<'_, Arc<crate::agent::ToolState>>,
    context_state: State<'_, ContextState>,
    knowledge_state: State<'_, Arc<KnowledgeState>>,
    root: String,
) -> Result<(), String> {
    let p = PathBuf::from(&root);
    if !p.is_dir() {
        return Err(format!("Not a directory: {}", p.display()));
    }
    let changed = {
        let guard = state.workspace.lock().await;
        guard.first() != Some(&p)
    };
    if changed {
        state.lsp.shutdown_all();
    }
    state.workspace.lock().await.clear();
    state.workspace.lock().await.push(p.clone());
    let config_dir = app_config_dir(&app);
    sync_modes(&p);
    let _ = knowledge_state.scan(&p, &config_dir);
    sync_knowledge(&context_state, &knowledge_state).await;
    let _ = app.emit("agent-knowledge", knowledge_report(&knowledge_state));
    Ok(())
}

/// Get all configured workspace roots.
#[tauri::command]
pub async fn agent_get_workspaces(
    state: State<'_, Arc<crate::agent::ToolState>>,
) -> Result<Vec<String>, String> {
    let guard = state.workspace.lock().await;
    Ok(guard
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect())
}

/// Add a workspace root (multi-root support). Scans skills/rules from it.
#[tauri::command]
pub async fn agent_add_workspace(
    app: AppHandle,
    state: State<'_, Arc<crate::agent::ToolState>>,
    context_state: State<'_, ContextState>,
    knowledge_state: State<'_, Arc<KnowledgeState>>,
    root: String,
) -> Result<Vec<String>, String> {
    let p = PathBuf::from(&root);
    if !p.is_dir() {
        return Err(format!("Not a directory: {}", p.display()));
    }
    let mut guard = state.workspace.lock().await;
    if !guard.contains(&p) {
        guard.push(p.clone());
    }
    let workspaces: Vec<String> = guard
        .iter()
        .map(|w| w.to_string_lossy().into_owned())
        .collect();
    drop(guard);
    sync_modes(&p);
    let config_dir = app_config_dir(&app);
    let _ = knowledge_state.scan(&p, &config_dir);
    sync_knowledge(&context_state, &knowledge_state).await;
    let _ = app.emit("agent-knowledge", knowledge_report(&knowledge_state));
    Ok(workspaces)
}

/// Remove a workspace root. The primary (first) workspace cannot be removed
/// via this call — use `agent_set_workspace` to replace it.
#[tauri::command]
pub async fn agent_remove_workspace(
    app: AppHandle,
    state: State<'_, Arc<crate::agent::ToolState>>,
    context_state: State<'_, ContextState>,
    knowledge_state: State<'_, Arc<KnowledgeState>>,
    root: String,
) -> Result<Vec<String>, String> {
    let p = PathBuf::from(&root);
    let mut guard = state.workspace.lock().await;
    guard.retain(|w| w != &p);
    let workspaces: Vec<String> = guard
        .iter()
        .map(|w| w.to_string_lossy().into_owned())
        .collect();
    drop(guard);
    let config_dir = app_config_dir(&app);
    if let Some(primary) = state.workspace.lock().await.first().cloned() {
        let _ = knowledge_state.scan(&primary, &config_dir);
    }
    sync_knowledge(&context_state, &knowledge_state).await;
    let _ = app.emit("agent-knowledge", knowledge_report(&knowledge_state));
    Ok(workspaces)
}

/// Get the currently configured primary workspace root (if any).
#[tauri::command]
pub async fn agent_get_workspace(
    state: State<'_, Arc<crate::agent::ToolState>>,
) -> Result<Option<String>, String> {
    let guard = state.workspace.lock().await;
    Ok(guard.first().map(|p| p.to_string_lossy().into_owned()))
}
