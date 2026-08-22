//! Persistent todo-list behind the `set_todo_list` / `get_todo_list` /
//! `mark_todo_item_done` tools (Bionic §3.2 PLANNING; roadmap P1-7 goals &
//! todos).
//!
//! The authoritative state lives in `{workspace}/.ai/todos.json`. Every
//! mutation rewrites the file and emits `agent://todo-update` so the UI renders
//! the checklist live. The orchestrator refuses to let a session finish while
//! items remain open (see `orchestrator::run_focused_steps`).

use std::path::Path;

use serde::{Deserialize, Serialize};

use super::now_ms;

/// Authoritative todo state file.
pub const TODOS_JSON: &str = ".ai/todos.json";

/// One todo entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TodoItem {
    /// 1-based stable index (position in the list).
    pub id: usize,
    pub title: String,
    #[serde(default)]
    pub done: bool,
}

/// The session's todo list.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TodoList {
    #[serde(default)]
    pub items: Vec<TodoItem>,
    #[serde(default)]
    pub updated_at: u64,
}

impl TodoList {
    /// Number of still-open (not done) items.
    pub fn open_count(&self) -> usize {
        self.items.iter().filter(|i| !i.done).count()
    }

    /// Render as a markdown checklist (also used for the model-facing view).
    pub fn render(&self) -> String {
        if self.items.is_empty() {
            return "(the todo list is empty)".to_string();
        }
        let mut out = String::new();
        for item in &self.items {
            out.push_str(if item.done { "- [x] " } else { "- [ ] " });
            out.push_str(&item.title);
            out.push('\n');
        }
        out.push_str(&format!(
            "\n{} of {} done.",
            self.items.len() - self.open_count(),
            self.items.len()
        ));
        out
    }

    /// Write the authoritative JSON. Creates `.ai` as needed.
    pub fn save(&self, workspace: &Path) -> Result<(), String> {
        let dir = workspace.join(".ai");
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("Failed to create {}: {e}", dir.display()))?;
        let text = serde_json::to_string_pretty(self)
            .map_err(|e| format!("Failed to serialize todos: {e}"))?;
        std::fs::write(dir.join("todos.json"), text)
            .map_err(|e| format!("Failed to write todos.json: {e}"))
    }

    /// Load the todo list for a workspace, if any.
    pub fn load(workspace: &Path) -> Option<TodoList> {
        let text = std::fs::read_to_string(workspace.join(TODOS_JSON)).ok()?;
        serde_json::from_str(&text).ok()
    }
}

/// Build a fresh list from `set_todo_list` arguments (everything starts open).
pub fn new_list(items: Vec<String>) -> TodoList {
    let clean: Vec<String> = items
        .into_iter()
        .map(|i| i.trim().to_string())
        .filter(|i| !i.is_empty())
        .collect();
    TodoList {
        items: clean
            .iter()
            .enumerate()
            .map(|(i, title)| TodoItem {
                id: i + 1,
                title: title.clone(),
                done: false,
            })
            .collect(),
        updated_at: now_ms(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_list_trims_and_numbers() {
        let l = new_list(vec!["  a ".into(), "".into(), "b".into()]);
        assert_eq!(l.items.len(), 2);
        assert_eq!(l.items[0].id, 1);
        assert_eq!(l.items[0].title, "a");
        assert_eq!(l.items[1].id, 2);
        assert_eq!(l.open_count(), 2);
    }

    #[test]
    fn render_shows_checkboxes_and_progress() {
        let mut l = new_list(vec!["one".into(), "two".into()]);
        l.items[0].done = true;
        let md = l.render();
        assert!(md.contains("- [x] one"));
        assert!(md.contains("- [ ] two"));
        assert!(md.contains("1 of 2 done"));
    }

    #[test]
    fn roundtrips_through_disk() {
        let dir = std::env::temp_dir().join(format!("todos-test-{}", std::process::id()));
        let l = new_list(vec!["alpha".into(), "beta".into()]);
        l.save(&dir).unwrap();
        let loaded = TodoList::load(&dir).unwrap();
        assert_eq!(loaded.items.len(), 2);
        assert_eq!(loaded.items[1].title, "beta");
        assert!(!loaded.items[1].done);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
