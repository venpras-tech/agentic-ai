//! Persistent plan file support behind the `create_plan` / `read_plan` /
//! `update_plan` / `execute_plan` tools (§11 of the blueprint).
//!
//! The authoritative state lives in `.ai/plan.json`; `.ai/plan.md` is a
//! rendered, human-editable view of the same data (statuses included). Every
//! mutation rewrites both files so the two never drift.

use std::path::Path;

use serde::{Deserialize, Serialize};

use super::now_ms;

/// Authoritative plan state (JSON).
pub const PLAN_JSON: &str = ".ai/plan.json";
/// Rendered, human-editable view (Markdown).
pub const PLAN_MD: &str = ".ai/plan.md";

/// Status of a single plan item.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanStatus {
    #[default]
    NotStarted,
    InProgress,
    Completed,
    Terminal,
}

impl PlanStatus {
    pub fn label(&self) -> &'static str {
        match self {
            PlanStatus::NotStarted => "not_started",
            PlanStatus::InProgress => "in_progress",
            PlanStatus::Completed => "completed",
            PlanStatus::Terminal => "terminal",
        }
    }

    /// Accept tolerant spellings so a slightly-misbehaving model still works.
    pub fn from_label(s: &str) -> Option<PlanStatus> {
        match s {
            "not_started" | "pending" | "todo" | "planned" => Some(PlanStatus::NotStarted),
            "in_progress" | "running" => Some(PlanStatus::InProgress),
            "completed" | "done" | "complete" => Some(PlanStatus::Completed),
            "terminal" | "failed" | "abandoned" | "blocked" | "cancelled" | "canceled" => {
                Some(PlanStatus::Terminal)
            }
            _ => None,
        }
    }

    /// Checkbox marker used in the rendered markdown (`[ ]` / `[~]` / `[x]` /
    /// `[!]`). The markdown is a view, not the source of truth, so non-standard
    /// markers are acceptable for readability.
    fn marker(&self) -> &'static str {
        match self {
            PlanStatus::NotStarted => " ",
            PlanStatus::InProgress => "~",
            PlanStatus::Completed => "x",
            PlanStatus::Terminal => "!",
        }
    }
}

/// One item of a plan.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanItem {
    pub id: usize,
    pub title: String,
    #[serde(default)]
    pub details: String,
    #[serde(default)]
    pub status: PlanStatus,
}

/// The active plan for the workspace.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanState {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub goal: String,
    pub items: Vec<PlanItem>,
    pub created_at: u64,
    pub updated_at: u64,
}

impl PlanState {
    /// Render the plan as a human-editable Markdown file.
    pub fn render_markdown(&self) -> String {
        let mut out = format!("# {}\n\n", self.title);
        if !self.goal.trim().is_empty() {
            out.push_str("> ");
            out.push_str(self.goal.trim());
            out.push_str("\n\n");
        }
        out.push_str(&format!("<!-- plan id: {} -->\n\n", self.id));
        out.push_str("| # | Status | Item |\n|--:|--------|------|\n");
        for (i, item) in self.items.iter().enumerate() {
            out.push_str(&format!(
                "| {} | {} | **{}**{} |\n",
                i + 1,
                item.status.label().replace('_', " "),
                item.title,
                if item.details.trim().is_empty() {
                    String::new()
                } else {
                    format!(" — {}", item.details.trim())
                }
            ));
        }
        out.push('\n');
        out.push_str("### Progress\n\n");
        let done = self.items.iter().filter(|i| i.status == PlanStatus::Completed).count();
        out.push_str(&format!(
            "- Completed: {}/{} items\n",
            done,
            self.items.len()
        ));
        out.push_str(&format!("- Updated: {}\n", self.updated_at));
        out
    }

    /// Write both the authoritative JSON and the rendered Markdown. Creates the
    /// `.ai` directory as needed.
    pub fn save(&self, workspace: &Path) -> Result<(), String> {
        let dir = workspace.join(".ai");
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("Failed to create {}: {e}", dir.display()))?;
        let json_text = serde_json::to_string_pretty(self)
            .map_err(|e| format!("Failed to serialize plan: {e}"))?;
        std::fs::write(dir.join("plan.json"), json_text)
            .map_err(|e| format!("Failed to write plan.json: {e}"))?;
        std::fs::write(dir.join("plan.md"), self.render_markdown())
            .map_err(|e| format!("Failed to write plan.md: {e}"))?;
        Ok(())
    }

    /// Load the authoritative plan state for a workspace, if any.
    pub fn load(workspace: &Path) -> Option<PlanState> {
        let text = std::fs::read_to_string(workspace.join(PLAN_JSON)).ok()?;
        serde_json::from_str(&text).ok()
    }
}

/// Build a fresh [`PlanState`] from `create_plan` arguments.
pub fn new_plan(title: &str, goal: &str, items: Vec<String>) -> PlanState {
    let now = now_ms();
    let clean: Vec<String> = items
        .into_iter()
        .map(|i| i.trim().to_string())
        .filter(|i| !i.is_empty())
        .collect();
    let items = clean
        .iter()
        .enumerate()
        .map(|(i, title)| PlanItem {
            id: i + 1,
            title: title.clone(),
            details: String::new(),
            status: PlanStatus::NotStarted,
        })
        .collect();
    PlanState {
        id: format!("plan-{now:x}"),
        title: title.trim().to_string(),
        goal: goal.trim().to_string(),
        items,
        created_at: now,
        updated_at: now,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_status_labels() {
        assert_eq!(PlanStatus::from_label("in_progress"), Some(PlanStatus::InProgress));
        assert_eq!(PlanStatus::from_label("done"), Some(PlanStatus::Completed));
        assert_eq!(PlanStatus::from_label("terminal"), Some(PlanStatus::Terminal));
        assert_eq!(PlanStatus::from_label("nonsense"), None);
    }

    #[test]
    fn renders_and_roundtrips() {
        let dir = std::env::temp_dir().join(format!("plan-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let p = new_plan(
            "Refactor auth",
            "Replace the hand-rolled login with a library.",
            vec!["Inspect current auth".into(), "Pick a library".into(), "Migrate".into()],
        );
        p.save(&dir).unwrap();
        let loaded = PlanState::load(&dir).unwrap();
        assert_eq!(loaded.items.len(), 3);
        assert_eq!(loaded.items[0].title, "Inspect current auth");
        assert_eq!(loaded.items[0].status, PlanStatus::NotStarted);
        let md = std::fs::read_to_string(dir.join(PLAN_MD)).unwrap();
        assert!(md.contains("# Refactor auth"));
        assert!(md.contains("Inspect current auth"));
        assert!(md.contains("0/3 items"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
