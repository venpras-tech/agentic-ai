//! Filesystem watcher for auto-reload on external workspace changes.
//!
//! Watches the workspace directory for create/modify/delete events and
//! emits `workspace://file-changed` Tauri events so the frontend can refresh
//! the file explorer. Respects `.gitignore` rules to skip ignored paths.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use ignore::gitignore::GitignoreBuilder;
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use serde::Serialize;
use tauri::{AppHandle, Emitter};
use tokio_util::sync::CancellationToken;

/// Debounce interval: collapse rapid filesystem events into one frontend refresh.
const DEBOUNCE_MS: u64 = 300;

/// Payload emitted to the frontend on workspace changes.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceChangedEvent {
    /// The kind of change: "create", "modify", "remove", or "any".
    pub kind: String,
    /// Relative paths that changed (relative to workspace root).
    pub paths: Vec<String>,
}

/// Shared state for the file watcher.
pub struct WatcherState {
    /// The currently-watched workspace root (if any), wrapped in Arc for thread-safe cloning.
    watched_path: Arc<Mutex<Option<PathBuf>>>,
    /// Cancellation token for the debounce task.
    cancel: Mutex<Option<CancellationToken>>,
}

impl WatcherState {
    pub fn new() -> Self {
        Self {
            watched_path: Arc::new(Mutex::new(None)),
            cancel: Mutex::new(None),
        }
    }
}

/// Start watching a workspace directory. Stops any previous watcher first.
#[tauri::command]
pub fn start_file_watcher(
    app: AppHandle,
    state: tauri::State<'_, WatcherState>,
    path: String,
) -> Result<(), String> {
    let root = PathBuf::from(&path);
    if !root.is_dir() {
        return Err(format!("Path is not a directory: {path}"));
    }

    // Stop previous watcher if any.
    stop_file_watcher_inner(&state);

    // Create the notify watcher.
    let (tx, rx) = std::sync::mpsc::channel::<notify::Result<Event>>();

    let mut watcher = RecommendedWatcher::new(
        tx,
        notify::Config::default()
            .with_compare_contents(false)
            .with_poll_interval(std::time::Duration::from_secs(2)),
    )
    .map_err(|e| format!("Failed to create file watcher: {e}"))?;

    watcher
        .watch(&root, RecursiveMode::Recursive)
        .map_err(|e| format!("Failed to watch directory: {e}"))?;

    // Cancel any previous debounce task.
    {
        let mut prev = state.cancel.lock().unwrap();
        if let Some(token) = prev.take() {
            token.cancel();
        }
    }

    let cancel = CancellationToken::new();
    {
        let mut cancel_guard = state.cancel.lock().unwrap();
        *cancel_guard = Some(cancel.clone());
    }

    // Build a gitignore matcher from the workspace root before moving it.
    let gi = {
        let mut builder = GitignoreBuilder::new(&root);
        let _ = builder.add(root.join(".gitignore"));
        let _ = builder.add(root.join(".ai").join(".gitignore"));
        builder.build().ok()
    };

    {
        let mut watched = state.watched_path.lock().unwrap();
        *watched = Some(root);
    }

    // Spawn the debounce + emit loop on a background thread.
    // Clone Arc handles for thread-safe shared access.
    let app_handle = app.clone();
    let cancel_clone = cancel;
    let watched_clone = Arc::clone(&state.watched_path);

    std::thread::Builder::new()
        .name("file-watcher".into())
        .spawn(move || {
            use std::sync::mpsc::RecvTimeoutError;
            use std::time::Duration;

            let debounce = Duration::from_millis(DEBOUNCE_MS);
            let mut pending_paths: Vec<PathBuf> = Vec::new();
            let mut pending_kind: Option<EventKind> = None;

            loop {
                if cancel_clone.is_cancelled() {
                    break;
                }

                match rx.recv_timeout(debounce) {
                    Ok(Ok(event)) => {
                        match event.kind {
                            EventKind::Create(_)
                            | EventKind::Modify(_)
                            | EventKind::Remove(_) => {
                                pending_paths.extend(event.paths);
                                pending_kind = Some(event.kind);
                            }
                            _ => {}
                        }
                    }
                    Ok(Err(_)) => {}
                    Err(RecvTimeoutError::Timeout) => {
                        // Flush pending events.
                        if let Some(kind) = pending_kind.take() {
                            let kind_str = match kind {
                                EventKind::Create(_) => "create",
                                EventKind::Modify(_) => "modify",
                                EventKind::Remove(_) => "remove",
                                _ => "any",
                            };
                            let root_lock = watched_clone.lock().unwrap();
                            let root_ref = root_lock.as_ref();
                            let relative: Vec<String> = pending_paths
                                .iter()
                                .filter_map(|p| {
                                    root_ref.and_then(|r| p.strip_prefix(r).ok())
                                })
                                .filter(|rel| {
                                    // Filter paths matched by .gitignore.
                                    if let Some(ref gi) = gi {
                                        !gi.matched_path_or_any_parents(rel, rel.is_dir()).is_ignore()
                                    } else {
                                        true
                                    }
                                })
                                .map(|p| p.to_string_lossy().into_owned())
                                .collect();
                            pending_paths.clear();

                            if !relative.is_empty() {
                                let _ = app_handle.emit(
                                    "workspace://file-changed",
                                    WorkspaceChangedEvent {
                                        kind: kind_str.to_string(),
                                        paths: relative,
                                    },
                                );
                            }
                        }
                    }
                    Err(RecvTimeoutError::Disconnected) => break,
                }
            }
        })
        .map_err(|e| format!("Failed to spawn watcher thread: {e}"))?;

    Ok(())
}

/// Stop watching the workspace directory.
#[tauri::command]
pub fn stop_file_watcher(state: tauri::State<'_, WatcherState>) {
    stop_file_watcher_inner(&state);
}

fn stop_file_watcher_inner(state: &WatcherState) {
    // Cancel the debounce task (which holds the watcher alive via the rx channel).
    if let Some(token) = state.cancel.lock().unwrap().take() {
        token.cancel();
    }
    // Clear the watched path.
    *state.watched_path.lock().unwrap() = None;
}

/// Check if the file watcher is active.
#[tauri::command]
pub fn file_watcher_active(state: tauri::State<'_, WatcherState>) -> bool {
    state.watched_path.lock().unwrap().is_some()
}
