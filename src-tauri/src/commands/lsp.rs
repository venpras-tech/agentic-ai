//! `lsp_diagnostics` — resolve real diagnostics from the language-server
//! client (`crate::lsp`), returning which server produced them so the editor
//! can distinguish "clean" from "nothing configured".

use std::sync::Arc;

use tauri::{AppHandle, State};

use crate::lsp::{LspDiagnostics, SharedLspManager};

#[tauri::command]
pub async fn lsp_diagnostics(
    app: AppHandle,
    state: State<'_, SharedLspManager>,
    path: String,
) -> Result<LspDiagnostics, String> {
    let mgr = Arc::clone(&*state);
    tokio::task::spawn_blocking(move || mgr.diagnostics(&app, &path))
        .await
        .map_err(|e| format!("diagnostics task failed: {e}"))
}
