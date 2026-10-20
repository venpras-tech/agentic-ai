use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use sha2::{Digest, Sha256};

use super::tools;
use super::{
    now_ms, EditBatchEvent, EditDecision, EditStatus, FileChangedEvent, FileCheckpoint, FileEdit,
    PendingEditGroup, ToolContext, ToolResult, ToolState,
};

/// Number of seconds a file-edit tool call waits for the user's review
/// (Apply / Reject) before timing out and reporting failure to the model.
const EDIT_REVIEW_TIMEOUT_SECS: u64 = 600;

/// Deserialized body of one proposed edit: exactly one of `patch`,
/// `search`+`replace`, or `full_content` drives the change.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileEditSpec {
    /// Workspace-relative or absolute path.
    pub path: String,
    #[serde(default)]
    pub line_start: Option<u64>,
    #[serde(default)]
    pub line_end: Option<u64>,
    #[serde(default)]
    pub patch: Option<String>,
    #[serde(default)]
    pub search: Option<String>,
    #[serde(default)]
    pub replace: Option<String>,
    #[serde(default)]
    pub full_content: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
}

/// Canonicalize `raw` against `root`. Relative paths must stay inside the
/// workspace; absolute paths pass through unchanged (they were already vetted
/// by the policy layer before the edit was proposed).
pub fn resolve_path(root: &Path, raw: &str) -> Result<PathBuf, String> {
    let p = PathBuf::from(raw);
    let absolute = p.is_absolute();
    let candidate = if absolute { p.clone() } else { root.join(&p) };
    let root_canon = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());

    let mut ancestor = candidate.as_path();
    let mut missing: Vec<std::ffi::OsString> = Vec::new();
    loop {
        match std::fs::canonicalize(ancestor) {
            Ok(c) => {
                let mut out = c;
                for seg in missing.iter().rev() {
                    out.push(seg);
                }
                if absolute {
                    return Ok(out);
                }
                if out.starts_with(&root_canon) {
                    return Ok(out);
                }
                return Err(format!(
                    "Path escapes the workspace `{}`: `{raw}`",
                    root_canon.display()
                ));
            }
            Err(_) => match ancestor.file_name() {
                Some(name) => {
                    missing.push(name.to_os_string());
                    match ancestor.parent() {
                        Some(parent) if parent.as_os_str().is_empty() => {
                            return Err(format!("Cannot resolve path `{raw}`"));
                        }
                        Some(parent) => ancestor = parent,
                        None => return Err(format!("Cannot resolve path `{raw}`")),
                    }
                }
                None => return Err(format!("Cannot resolve path `{raw}`")),
            },
        }
    }
}

/// Workspace-relative display path (forward slashes), falling back to the
/// absolute path for anything outside the workspace.
pub fn rel_path(root: Option<&Path>, path: &Path) -> String {
    match root.and_then(|r| path.strip_prefix(r).ok()) {
        Some(rel) => rel.to_string_lossy().replace('\\', "/"),
        None => path.to_string_lossy().replace('\\', "/"),
    }
}

/// sha256 of a file's text content, for conflict detection.
pub fn file_hash(content: &str) -> String {
    let mut h = Sha256::new();
    h.update(content.as_bytes());
    format!("{:x}", h.finalize())
}

/// Unified diff of before -> after; `None` when there is no change.
pub fn compute_unified_diff(before: &str, after: &str) -> Option<String> {
    if before == after {
        return None;
    }
    let d = similar::TextDiff::from_lines(before, after);
    Some(d.unified_diff().header("before", "after").to_string())
}

/// Snapshot path for a checkpoint id: `{workspace}/.ai/undo/<id>.snap`.
pub fn snapshot_path(root: &Path, id: &str) -> PathBuf {
    root.join(".ai").join("undo").join(format!("{id}.snap"))
}

/// Compute what the file should contain after applying `spec`, without writing.
pub fn content_to_apply(original: &str, spec: &FileEditSpec) -> Result<String, String> {
    if let Some(full) = spec.full_content.as_deref() {
        return Ok(full.to_string());
    }
    if let Some(patch) = spec.patch.as_deref() {
        if original.trim().is_empty() {
            return tools::apply_new_file(patch);
        }
        return tools::apply_unified_diff(original, patch);
    }
    if let Some(search) = spec.search.as_deref() {
        if search.is_empty() {
            return Err("`search` cannot be empty for a search/replace edit".into());
        }
        let replace = spec.replace.as_deref().unwrap_or("");
        let idx = original
            .find(search)
            .ok_or_else(|| format!("Search text not found in `{}`", spec.path))?;
        let mut out = String::with_capacity(original.len() + replace.len().max(search.len()));
        out.push_str(&original[..idx]);
        out.push_str(replace);
        out.push_str(&original[idx + search.len()..]);
        return Ok(out);
    }
    Err("Edit has no body (none of patch / search / full_content supplied)".into())
}

/// Register a proposed edit, block until the user reviews it, and run or abort.
///
/// The actual disk write happens in `apply` (driven by the user clicking Apply
/// in the review pane); this future only reports the outcome back to the tool.
#[allow(clippy::too_many_arguments)]
pub async fn propose_edit(
    app: &ToolContext,
    state: &ToolState,
    tool: &str,
    root: &Path,
    spec: &FileEditSpec,
    before: Option<String>,
) -> Result<ToolResult, String> {
    propose_edit_inner(
        app,
        state,
        tool,
        root,
        spec,
        before,
        Duration::from_secs(EDIT_REVIEW_TIMEOUT_SECS),
    )
    .await
}

/// Body of `propose_edit` with an injectable review timeout (the public entry
/// point uses `EDIT_REVIEW_TIMEOUT_SECS`; tests shrink it to milliseconds).
#[allow(clippy::too_many_arguments)]
pub async fn propose_edit_inner(
    app: &ToolContext,
    state: &ToolState,
    tool: &str,
    root: &Path,
    spec: &FileEditSpec,
    before: Option<String>,
    review_timeout: Duration,
) -> Result<ToolResult, String> {
    let before_text = before.clone().unwrap_or_default();
    let after = content_to_apply(&before_text, spec)?;
    let kind = match (&spec.full_content, &spec.patch, &spec.search) {
        (Some(_), _, _) => "full_content",
        (None, Some(_), _) => "patch",
        (None, None, Some(_)) => "search_replace",
        (None, None, None) => return Err("Edit has no body".into()),
    };

    let group_id = state.next_edit_group_id();
    let session_id = state.session_id.load(std::sync::atomic::Ordering::SeqCst);
    let edit = FileEdit {
        id: state.next_edit_id(),
        path: rel_path(Some(root), &resolve_path(root, &spec.path)?),
        line_start: spec.line_start,
        line_end: spec.line_end,
        kind: kind.to_string(),
        diff: compute_unified_diff(&before_text, &after),
        before,
        after: Some(after),
        reason: spec.reason.clone(),
        status: EditStatus::Pending,
        error: None,
    };

    let group = PendingEditGroup {
        id: group_id.clone(),
        session_id,
        tool: tool.to_string(),
        edits: vec![edit.clone()],
    };
    let (tx, rx) = tokio::sync::oneshot::channel();
    state
        .pending_edits
        .lock()
        .unwrap()
        .insert(group_id.clone(), group);
    state
        .edit_decisions
        .lock()
        .await
        .insert(group_id.clone(), tx);

    let _ = app.emit(
        "agent://edit-proposed",
        EditBatchEvent {
            session_id,
            group_id: group_id.clone(),
            edits: vec![edit],
            timestamp_ms: now_ms(),
        },
    );

    let decision = tokio::time::timeout(review_timeout, rx).await;

    match decision {
        Ok(Ok(EditDecision::Approve)) => {
            state.edit_decisions.lock().await.remove(&group_id);
            state.pending_edits.lock().unwrap().remove(&group_id);
            Ok(ToolResult::ok(
                tool,
                format!("Applied edit to `{}`", &spec.path),
                None,
                None,
            ))
        }
        Ok(Ok(EditDecision::Reject)) => {
            state.edit_decisions.lock().await.remove(&group_id);
            state.pending_edits.lock().unwrap().remove(&group_id);
            Ok(ToolResult::err(
                tool,
                "User rejected the edit".into(),
                format!("The proposed change to `{}` was declined.", &spec.path),
            ))
        }
        _ => {
            if let Some(tx) = state.edit_decisions.lock().await.remove(&group_id) {
                let _ = tx.send(EditDecision::Reject);
            }
            if let Some(group) = state.pending_edits.lock().unwrap().remove(&group_id) {
                let edits: Vec<FileEdit> = group
                    .edits
                    .into_iter()
                    .map(|mut e| {
                        e.status = EditStatus::Failed;
                        e.error = Some("Timed out waiting for review".into());
                        e
                    })
                    .collect();
                let _ = app.emit(
                    "agent://edit-failed",
                    EditBatchEvent {
                        session_id,
                        group_id,
                        edits,
                        timestamp_ms: now_ms(),
                    },
                );
            }
            Ok(ToolResult::err(
                tool,
                "Edit review timed out".into(),
                "The proposed edit expired before a decision was made.".into(),
            ))
        }
    }
}

/// Apply one already-approved edit group: conflict-check, snapshot, write, and
/// emit the outcome events. Returns the per-edit statuses afterwards. If no
/// decision channel exists (e.g. the proposing run was aborted), still applies
/// because the user explicitly clicked Apply.
pub async fn apply_edits(
    app: &ToolContext,
    state: &ToolState,
    group_id: &str,
    edit_ids: &[String],
) -> Result<Vec<FileEdit>, String> {
    let root = state
        .primary_workspace()
        .await
        .ok_or_else(|| "No workspace set".to_string())?;
    let group = state
        .pending_edits
        .lock()
        .unwrap()
        .get(group_id)
        .cloned()
        .ok_or_else(|| format!("No pending edit group `{group_id}`"))?;

    let mut edits = Vec::new();
    let mut applied_any = false;
    for mut edit in group.edits {
        if !edit_ids.is_empty() && !edit_ids.contains(&edit.id) {
            continue;
        }
        let abs = match resolve_path(&root, &edit.path) {
            Ok(p) => p,
            Err(e) => {
                edit.status = EditStatus::Failed;
                edit.error = Some(e);
                edits.push(edit);
                continue;
            }
        };
        let disk_before = std::fs::read_to_string(&abs).ok();
        let conflict = match (&disk_before, &edit.before) {
            (None, Some(_)) => true,
            (Some(cur), Some(before)) => file_hash(cur) != file_hash(before),
            _ => false,
        };
        if conflict {
            edit.status = EditStatus::Conflict;
            edit.error = Some(format!(
                "File changed on disk since the edit was proposed (`{}` differs).",
                edit.path
            ));
            edits.push(edit);
            continue;
        }
        let Some(after) = edit.after.clone() else {
            edit.status = EditStatus::Failed;
            edit.error = Some("Edit has no computed content".into());
            edits.push(edit);
            continue;
        };
        if let Some(parent) = abs.parent() {
            if std::fs::create_dir_all(parent).is_err() {
                edit.status = EditStatus::Failed;
                edit.error = Some(format!("Could not create directory `{}`", parent.display()));
                edits.push(edit);
                continue;
            }
        }
        let before_text = edit.before.clone().unwrap_or_default();
        let checkpoint_id = state.next_checkpoint_id();
        let snap = snapshot_path(&root, &checkpoint_id);
        if std::fs::create_dir_all(snap.parent().unwrap_or(&root)).is_ok() {
            let _ = std::fs::write(&snap, &before_text);
        }
        if std::fs::write(&abs, &after).is_err() {
            edit.status = EditStatus::Failed;
            edit.error = Some(format!("Failed to write `{}`", edit.path));
            edits.push(edit);
            continue;
        }
        state.file_checkpoints.lock().unwrap().push(FileCheckpoint {
            id: checkpoint_id,
            path: edit.path.clone(),
            timestamp_ms: now_ms(),
            edit_id: edit.id.clone(),
            content_hash: file_hash(&before_text),
        });
        let _ = app.emit(
            "agent://file-changed",
            FileChangedEvent {
                path: edit.path.clone(),
                kind: "diff".into(),
                diff: edit.diff.clone(),
                before: edit.before.clone(),
            },
        );
        edit.status = EditStatus::Applied;
        edits.push(edit);
        applied_any = true;
    }

    if applied_any {
        let _ = app.emit(
            "agent://edit-applied",
            EditBatchEvent {
                session_id: group.session_id,
                group_id: group_id.to_string(),
                edits: edits.clone(),
                timestamp_ms: now_ms(),
            },
        );
    } else {
        let _ = app.emit(
            "agent://edit-failed",
            EditBatchEvent {
                session_id: group.session_id,
                group_id: group_id.to_string(),
                edits: edits.clone(),
                timestamp_ms: now_ms(),
            },
        );
    }

    if let Ok(mut groups) = state.pending_edits.lock() {
        if let Some(stored) = groups.get_mut(group_id) {
            stored.edits = edits.clone();
        }
    }
    if let Some(tx) = state.edit_decisions.lock().await.remove(group_id) {
        let _ = tx.send(EditDecision::Approve);
    }

    Ok(edits)
}

/// Reject a pending edit group (or selected edits within it) and unblock the
/// tool call that proposed it.
pub async fn reject_edits(
    state: &ToolState,
    app: &ToolContext,
    group_id: &str,
    edit_ids: &[String],
    reason: Option<String>,
) -> Result<Vec<FileEdit>, String> {
    let edits = {
        let mut groups = state.pending_edits.lock().unwrap();
        let Some(group) = groups.get_mut(group_id) else {
            return Err(format!("No pending edit group `{group_id}`"));
        };
        for edit in group.edits.iter_mut() {
            if !edit_ids.is_empty() && !edit_ids.contains(&edit.id) {
                continue;
            }
            edit.status = EditStatus::Rejected;
            edit.error = reason.clone();
        }
        group.edits.clone()
    };
    let _ = app.emit(
        "agent://edit-applied",
        EditBatchEvent {
            session_id: state.session_id.load(std::sync::atomic::Ordering::SeqCst),
            group_id: group_id.to_string(),
            edits: edits.clone(),
            timestamp_ms: now_ms(),
        },
    );
    if let Some(tx) = state.edit_decisions.lock().await.remove(group_id) {
        let _ = tx.send(EditDecision::Reject);
    }
    Ok(edits)
}

/// Resolve conflict-status edits in one group. Two actions:
/// - `overwrite=true`: write the proposed `after` content to disk anyway. A
///   pre-apply checkpoint of the *current* on-disk content is taken exactly like
///   a normal apply, so a restore brings back what the overwrite replaced, then
///   the edits are marked `Applied`.
/// - `overwrite=false` ("reload from disk"): keep the on-disk file untouched
///   and mark the edits `Rejected` with an explanatory reason, so the proposing
///   tool re-proposes against the current content.
/// Only edits in `edit_ids` (all of them when empty) and only those currently in
/// `Conflict` are touched; everything else passes through unchanged.
/// Returns the per-edit statuses afterwards.
pub async fn resolve_conflict_edits(
    app: &ToolContext,
    state: &ToolState,
    group_id: &str,
    overwrite: bool,
    edit_ids: &[String],
) -> Result<Vec<FileEdit>, String> {
    let root = state
        .primary_workspace()
        .await
        .ok_or_else(|| "No workspace set".to_string())?;
    let group = state
        .pending_edits
        .lock()
        .unwrap()
        .get(group_id)
        .cloned()
        .ok_or_else(|| format!("No pending edit group `{group_id}`"))?;

    let mut edits = Vec::new();
    let mut applied_any = false;
    for mut edit in group.edits {
        if !edit_ids.is_empty() && !edit_ids.contains(&edit.id) {
            // Not part of this action: keep it exactly as it is, so a group
            // with several conflicts stays fully actionable.
            edits.push(edit);
            continue;
        }
        if edit.status != EditStatus::Conflict {
            edits.push(edit);
            continue;
        }
        if !overwrite {
            edit.status = EditStatus::Rejected;
            edit.error = Some(
                "File reloaded from disk — re-apply the edit against the current file content."
                    .into(),
            );
            edits.push(edit);
            continue;
        }
        let abs = match resolve_path(&root, &edit.path) {
            Ok(p) => p,
            Err(e) => {
                edit.status = EditStatus::Failed;
                edit.error = Some(e);
                edits.push(edit);
                continue;
            }
        };
        let Some(after) = edit.after.clone() else {
            edit.status = EditStatus::Failed;
            edit.error = Some("Edit has no computed content".into());
            edits.push(edit);
            continue;
        };
        if let Some(parent) = abs.parent() {
            if std::fs::create_dir_all(parent).is_err() {
                edit.status = EditStatus::Failed;
                edit.error = Some(format!("Could not create directory `{}`", parent.display()));
                edits.push(edit);
                continue;
            }
        }
        // Overwriting discards whatever is on disk right now, so that is what a
        // restore must bring back — not the (stale) content the edit was
        // proposed against.
        let disk_before = std::fs::read_to_string(&abs).unwrap_or_default();
        let checkpoint_id = state.next_checkpoint_id();
        let snap = snapshot_path(&root, &checkpoint_id);
        if std::fs::create_dir_all(snap.parent().unwrap_or(&root)).is_ok() {
            let _ = std::fs::write(&snap, &disk_before);
        }
        if std::fs::write(&abs, &after).is_err() {
            edit.status = EditStatus::Failed;
            edit.error = Some(format!("Failed to write `{}`", edit.path));
            edits.push(edit);
            continue;
        }
        state.file_checkpoints.lock().unwrap().push(FileCheckpoint {
            id: checkpoint_id,
            path: edit.path.clone(),
            timestamp_ms: now_ms(),
            edit_id: edit.id.clone(),
            content_hash: file_hash(&disk_before),
        });
        let _ = app.emit(
            "agent://file-changed",
            FileChangedEvent {
                path: edit.path.clone(),
                kind: "diff".into(),
                diff: edit.diff.clone(),
                before: Some(disk_before),
            },
        );
        edit.status = EditStatus::Applied;
        edits.push(edit);
        applied_any = true;
    }

    let _ = app.emit(
        "agent://edit-applied",
        EditBatchEvent {
            session_id: group.session_id,
            group_id: group_id.to_string(),
            edits: edits.clone(),
            timestamp_ms: now_ms(),
        },
    );
    if let Ok(mut groups) = state.pending_edits.lock() {
        if let Some(stored) = groups.get_mut(group_id) {
            stored.edits = edits.clone();
        }
    }
    if let Some(tx) = state.edit_decisions.lock().await.remove(group_id) {
        let _ = tx.send(if applied_any {
            EditDecision::Approve
        } else {
            EditDecision::Reject
        });
    }

    Ok(edits)
}

/// All currently pending edit groups (for `list_pending_edits`).
pub fn list_pending_edits(state: &ToolState) -> HashMap<String, PendingEditGroup> {
    state.pending_edits.lock().unwrap().clone()
}

/// Snapshot content back to disk for one or more checkpoint ids.
pub async fn restore_checkpoints(
    app: &ToolContext,
    state: &ToolState,
    ids: &[String],
) -> Result<usize, String> {
    let root = state
        .primary_workspace()
        .await
        .ok_or_else(|| "No workspace set".to_string())?;
    let checkpoints: Vec<FileCheckpoint> = {
        let all = state.file_checkpoints.lock().unwrap();
        all.iter()
            .filter(|c| ids.is_empty() || ids.contains(&c.id))
            .cloned()
            .collect()
    };
    if checkpoints.is_empty() && !ids.is_empty() {
        return Err("No matching checkpoint(s) found".into());
    }
    let mut restored = 0usize;
    for cp in checkpoints {
        let snap = snapshot_path(&root, &cp.id);
        let Ok(content) = std::fs::read_to_string(&snap) else {
            return Err(format!("Snapshot file missing for `{}`", cp.id));
        };
        let abs = resolve_path(&root, &cp.path)?;
        if let Some(parent) = abs.parent() {
            if std::fs::create_dir_all(parent).is_err() {
                return Err(format!("Could not create directory `{}`", parent.display()));
            }
        }
        if std::fs::write(&abs, &content).is_err() {
            return Err(format!("Failed to restore `{}`", cp.path));
        }
        let _ = app.emit(
            "agent://file-changed",
            FileChangedEvent {
                path: cp.path.clone(),
                kind: "write".into(),
                diff: None,
                before: Some(content),
            },
        );
        restored += 1;
    }
    Ok(restored)
}

/// All checkpoints in the current session.
pub fn list_checkpoints(state: &ToolState) -> Vec<FileCheckpoint> {
    let mut all = state.file_checkpoints.lock().unwrap().clone();
    all.sort_by_key(|c| std::cmp::Reverse(c.timestamp_ms));
    all
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn spec(path: &str) -> FileEditSpec {
        FileEditSpec {
            path: path.to_string(),
            line_start: None,
            line_end: None,
            patch: None,
            search: None,
            replace: None,
            full_content: None,
            reason: None,
        }
    }

    #[test]
    fn relative_path_resolves_inside_workspace() {
        let root = std::env::temp_dir().join("ai-edits-test-rel");
        std::fs::create_dir_all(&root).unwrap();
        let p = resolve_path(&root, "src/lib.rs").unwrap();
        assert!(p.starts_with(std::fs::canonicalize(&root).unwrap()));
        assert!(p.ends_with("lib.rs"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn traversal_escapes_workspace() {
        let root = std::env::temp_dir().join("ai-edits-test-traversal");
        std::fs::create_dir_all(&root).unwrap();
        let err = resolve_path(&root, "../secret.txt").unwrap_err();
        assert!(err.contains("escapes"), "{err}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn patch_edit_applies() {
        let original = "fn main() {\n    println!(\"hi\");\n}\n";
        let diff = "@@ -1,3 +1,3 @@\n fn main() {\n-    println!(\"hi\");\n+    println!(\"hello\");\n }\n";
        let mut s = spec("main.rs");
        s.patch = Some(diff.to_string());
        let after = content_to_apply(original, &s).unwrap();
        assert!(after.contains("hello"));
        assert!(!after.contains("hi"));
    }

    #[test]
    fn search_replace_replaces_first_occurrence() {
        let mut s = spec("x.txt");
        s.search = Some("foo".into());
        s.replace = Some("bar".into());
        let after = content_to_apply("foo foo foo", &s).unwrap();
        assert_eq!(after, "bar foo foo");
    }

    #[test]
    fn search_replace_requires_match() {
        let mut s = spec("x.txt");
        s.search = Some("nope".into());
        let err = content_to_apply("body", &s).unwrap_err();
        assert!(err.contains("not found"));
    }

    #[test]
    fn full_content_overrides_everything() {
        let mut s = spec("x.txt");
        s.full_content = Some("brand new".into());
        s.patch = Some("@@ -1 +1 @@\n-old\n+new\n".into());
        assert_eq!(content_to_apply("old", &s).unwrap(), "brand new");
    }

    #[test]
    fn hash_is_stable_and_differs() {
        let a = file_hash("hello");
        let b = file_hash("hello");
        let c = file_hash("hello!");
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn unified_diff_computed() {
        let d = compute_unified_diff("a\n", "b\n").expect("diff present");
        assert!(d.contains("@@"));
    }

    struct Sink;
    impl crate::agent::EventSink for Sink {
        fn emit_value(&self, _event: &str, _payload: &serde_json::Value) {}
    }

    fn ctx() -> ToolContext {
        ToolContext {
            sink: Arc::new(Sink),
            config_dir: std::env::temp_dir().join("ai-edits-test-ctx"),
        }
    }

    /// A module-scope uid so parallel tests never share a pending group.
    fn fresh_ctx() -> ToolContext {
        ctx()
    }

    #[tokio::test]
    async fn propose_reject_roundtrip_returns_ok() {
        let state = Arc::new(ToolState::default());
        let tmp = std::env::temp_dir().join("ai-edits-test-propose");
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("a.txt"), "hello").unwrap();
        let path = tmp.join("a.txt");
        let mut s = spec(&path.to_string_lossy());
        s.full_content = Some("goodbye".into());

        let state2 = state.clone();
        let s2 = s.clone();
        let tmp2 = tmp.clone();
        let handle = tokio::spawn(async move {
            propose_edit(
                &ctx(),
                &state2,
                "write_file",
                &tmp2,
                &s2,
                Some("hello".into()),
            )
            .await
        });

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let groups = list_pending_edits(&state);
        assert_eq!(groups.len(), 1, "must register a pending group");
        let gid = groups.keys().next().cloned().unwrap();
        reject_edits(&state, &fresh_ctx(), &gid, &[], Some("no thanks".into()))
            .await
            .unwrap();

        let res = handle.await.unwrap().unwrap();
        assert!(!res.success, "rejected edit must fail the tool call");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[tokio::test]
    async fn propose_timeout_fails_the_tool_call_and_clears_the_group() {
        let state = Arc::new(ToolState::default());
        let tmp = std::env::temp_dir().join("ai-edits-test-timeout");
        std::fs::create_dir_all(&tmp).unwrap();
        let path = tmp.join("a.txt");
        std::fs::write(&path, "hello").unwrap();
        let mut s = spec(&path.to_string_lossy());
        s.full_content = Some("goodbye".into());

        let res = propose_edit_inner(
            &ctx(),
            &state,
            "write_file",
            &tmp,
            &s,
            Some("hello".into()),
            Duration::from_millis(30),
        )
        .await
        .unwrap();

        assert!(!res.success, "timeout must fail the tool call");
        // The model-facing failure text = summary + error; assert it conveys
        // a user-review timeout.
        let combined =
            format!("{} {}", res.summary, res.error.as_deref().unwrap_or("")).to_lowercase();
        assert!(
            combined.contains("timed out") && combined.contains("review"),
            "model must receive a review-timeout failure text, got: {combined}"
        );
        assert_eq!(
            &res.summary, "Edit review timed out",
            "summary surfaces the failure to the model"
        );
        assert!(
            list_pending_edits(&state).is_empty(),
            "timeout must evict the pending group"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[tokio::test]
    async fn conflict_resolution_overwrites_or_keeps_disk() {
        let state = Arc::new(ToolState::default());
        let tmp = std::env::temp_dir().join("ai-edits-test-conflict");
        std::fs::create_dir_all(&tmp).unwrap();
        let path = tmp.join("a.txt");
        std::fs::write(&path, "v1").unwrap();
        state.workspace.lock().await.push(tmp.clone());

        let pending_gid = |state: &Arc<ToolState>| -> String {
            list_pending_edits(state)
                .into_values()
                .find(|g| g.edits.iter().any(|e| e.status == EditStatus::Pending))
                .expect("a pending group must exist")
                .id
        };

        // Cycle 1: "reload from disk" keeps the on-disk file and rejects.
        let mut s = spec(&path.to_string_lossy());
        s.full_content = Some("v2".into());
        let s2 = s.clone();
        let state2 = state.clone();
        let tmp2 = tmp.clone();
        let handle = tokio::spawn(async move {
            propose_edit(&ctx(), &state2, "write_file", &tmp2, &s2, Some("v1".into())).await
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        let gid = pending_gid(&state);
        std::fs::write(&path, "v3").unwrap();
        let applied = apply_edits(&fresh_ctx(), &state, &gid, &[]).await.unwrap();
        assert_eq!(applied[0].status, EditStatus::Conflict);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "v3");

        let resolved = resolve_conflict_edits(&fresh_ctx(), &state, &gid, false, &[])
            .await
            .unwrap();
        assert_eq!(resolved[0].status, EditStatus::Rejected);
        assert!(
            resolved[0]
                .error
                .as_deref()
                .unwrap_or("")
                .contains("reloaded"),
            "reload reason explains the action"
        );
        let _ = handle.await.unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "v3",
            "disk untouched"
        );

        // Cycle 2: overwrite force-writes the proposal and takes a checkpoint.
        let mut s = spec(&path.to_string_lossy());
        s.full_content = Some("v4".into());
        let s2 = s.clone();
        let state2 = state.clone();
        let tmp2 = tmp.clone();
        let handle = tokio::spawn(async move {
            propose_edit(&ctx(), &state2, "write_file", &tmp2, &s2, Some("v3".into())).await
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        let gid = pending_gid(&state);
        std::fs::write(&path, "v5").unwrap();
        let applied = apply_edits(&fresh_ctx(), &state, &gid, &[]).await.unwrap();
        assert_eq!(applied[0].status, EditStatus::Conflict);
        let resolved = resolve_conflict_edits(&fresh_ctx(), &state, &gid, true, &[])
            .await
            .unwrap();
        assert_eq!(resolved[0].status, EditStatus::Applied);
        let _ = handle.await.unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "v4",
            "overwrite wrote the proposal over the drifted disk content"
        );
        assert!(
            list_checkpoints(&state)
                .iter()
                .any(|c| c.content_hash == file_hash("v5")),
            "overwrite checkpoints the disk content it replaced (`v5`), not the proposal base"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Resolving one edit of a multi-conflict group must leave the other
    /// conflicts in place, both in the returned set and in stored state.
    #[tokio::test]
    async fn conflict_resolution_keeps_unselected_conflicts() {
        let state = Arc::new(ToolState::default());
        let tmp = std::env::temp_dir().join("ai-edits-test-conflict-multi");
        std::fs::create_dir_all(&tmp).unwrap();
        let path = tmp.join("a.txt");
        std::fs::write(&path, "v1").unwrap();
        state.workspace.lock().await.push(tmp.clone());

        let mut s = spec(&path.to_string_lossy());
        s.full_content = Some("v2".into());
        let s2 = s.clone();
        let state2 = state.clone();
        let tmp2 = tmp.clone();
        let handle = tokio::spawn(async move {
            propose_edit(&ctx(), &state2, "write_file", &tmp2, &s2, Some("v1".into())).await
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        let gid = list_pending_edits(&state)
            .into_values()
            .find(|g| g.edits.iter().any(|e| e.status == EditStatus::Pending))
            .expect("a pending group must exist")
            .id;
        std::fs::write(&path, "v3").unwrap();
        let applied = apply_edits(&fresh_ctx(), &state, &gid, &[]).await.unwrap();
        assert_eq!(applied[0].status, EditStatus::Conflict);
        let conflicting_id = applied[0].id.clone();

        // A second conflict edit is appended to the same group by hand, the way
        // a multi-file proposal would arrive.
        let mut group = state
            .pending_edits
            .lock()
            .unwrap()
            .get(&gid)
            .cloned()
            .unwrap();
        let mut second = group.edits[0].clone();
        second.id = "edit-second".into();
        second.status = EditStatus::Conflict;
        group.edits.push(second);
        state
            .pending_edits
            .lock()
            .unwrap()
            .get_mut(&gid)
            .unwrap()
            .edits = group.edits.clone();

        let resolved = resolve_conflict_edits(
            &fresh_ctx(),
            &state,
            &gid,
            true,
            std::slice::from_ref(&conflicting_id),
        )
        .await
        .unwrap();
        assert_eq!(resolved.len(), 2, "no edit is dropped from the group");
        assert_eq!(resolved[0].status, EditStatus::Applied);
        assert_eq!(resolved[1].status, EditStatus::Conflict, "unselected stays");

        let stored = state
            .pending_edits
            .lock()
            .unwrap()
            .get(&gid)
            .cloned()
            .unwrap();
        assert_eq!(stored.edits.len(), 2);
        assert_eq!(stored.edits[1].status, EditStatus::Conflict);
        let _ = handle.await.unwrap();
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
