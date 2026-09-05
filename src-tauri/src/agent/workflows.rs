//! User-defined workflows (`.ai/workflows/*.md`).
//!
//! A workflow is a reusable, named agentic recipe: a trigger command
//! (`/name`), a short description, a system-prompt directive, and an optional
//! tool allow-list. It is a *template* the user invokes on demand in chat.
//! File format (frontmatter + body):
//!
//! ```markdown
//! ---
//! name: release-prep
//! description: Bump version, update changelog, tag.
//! allowedTools: edit_file, run_tests, git_commit
//! ---
//! You are preparing a release. Update the changelog, bump the version in
//! package.json, run the tests, and commit — verifying after each step.
//! ```
//!
//! Invoking `/release-prep <goal>` sends the body directive + the user's goal
//! as the agent prompt. `allowedTools` (when non-empty) scopes the run to that
//! tool set via the same enforcement the custom-mode allow-lists use.

use std::path::Path;

/// A user-defined workflow loaded from a `.ai/workflows/*.md` file.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Workflow {
    /// Trigger name (`/name`) used to invoke the workflow.
    pub name: String,
    /// One-line description surfaced in the `/` hint menu.
    pub description: String,
    /// System-prompt directive; `""` when the file body is empty.
    pub system_prompt: String,
    /// Tool allow-list (`allowedTools:`), empty when unrestricted.
    pub allowed_tools: Vec<String>,
}

/// Load workflows from `.ai/workflows/*.md` files in the given workspace,
/// sorted by name.
pub fn load_workflows(workspace: &Path) -> Vec<Workflow> {
    let wf_dir = workspace.join(".ai").join("workflows");
    if !wf_dir.is_dir() {
        return Vec::new();
    }
    let mut workflows: Vec<Workflow> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&wf_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() && path.extension().map(|e| e == "md").unwrap_or(false) {
                if let Some(wf) = parse_workflow_file(&path) {
                    workflows.push(wf);
                }
            }
        }
    }
    workflows.sort_by(|a, b| a.name.cmp(&b.name));
    workflows
}

/// Build the effective user prompt for a workflow invocation:
/// `{directive}\n\nGoal: {goal}`.
pub fn prompt_for(workflow: &Workflow, goal: &str) -> String {
    let directive = if workflow.system_prompt.is_empty() {
        format!(
            "You are executing the `{}` workflow. Follow the workflow's steps carefully, \
             verify after each change, and report what you did.",
            workflow.name
        )
    } else {
        workflow.system_prompt.clone()
    };
    format!("{directive}\n\nGoal: {goal}")
}

/// Register a workflow's tool allow-list under `name` so the shared
/// `tool_allowed`/`child_verdict` enforcement gate applies for the run.
pub fn enforce_tools(name: &str, allowed_tools: Vec<String>) {
    super::subagent::register_workflow_tools(name, allowed_tools);
}

/// Resolve a tool-call verdict for a workflow-restricted child. `None` when no
/// workflow restriction is active for `name` (unrestricted or not registered).
pub fn workflow_verdict(name: &str, tool: &str) -> Option<String> {
    super::subagent::workflow_child_tool_verdict(name, tool)
}

fn parse_workflow_file(path: &Path) -> Option<Workflow> {
    let text = std::fs::read_to_string(path).ok()?;
    let (front, body) = split_frontmatter(&text)?;
    Some(Workflow {
        name: extract_yaml_field(&front, "name")?,
        description: extract_yaml_field(&front, "description").unwrap_or_default(),
        system_prompt: body.trim().to_string(),
        allowed_tools: extract_yaml_list(&front, "allowedTools"),
    })
}

/// Split `---\nkey: value\n---\nbody` frontmatter.
fn split_frontmatter(text: &str) -> Option<(String, String)> {
    let rest = text.strip_prefix("---")?;
    let end = rest.find("\n---")?;
    let front = rest[..end].to_string();
    let body = rest[end + 4..].trim_start().to_string();
    Some((front, body))
}

fn extract_yaml_field(frontmatter: &str, key: &str) -> Option<String> {
    for line in frontmatter.lines() {
        let line = line.trim();
        if let Some(v) = line.strip_prefix(&format!("{key}:")) {
            let v = v.trim().trim_matches('"').trim_matches('\'');
            return Some(v.to_string());
        }
    }
    None
}

fn extract_yaml_list(frontmatter: &str, key: &str) -> Vec<String> {
    match extract_yaml_list_opt(frontmatter, key) {
        Some(items) => items,
        None => Vec::new(),
    }
}

fn extract_yaml_list_opt(frontmatter: &str, key: &str) -> Option<Vec<String>> {
    let val = extract_yaml_field(frontmatter, key)?;
    if val.is_empty() {
        return Some(Vec::new());
    }
    let items: Vec<String> = val
        .split(',')
        .map(|s| s.trim().trim_matches('"').trim_matches('\'').to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if items.is_empty() {
        None
    } else {
        Some(items)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_workflows_returns_sorted_and_parses_prompt() {
        let dir = std::env::temp_dir().join(format!("ai-wf-sort-{}", std::process::id()));
        let wf_dir = dir.join(".ai").join("workflows");
        std::fs::create_dir_all(&wf_dir).unwrap();
        std::fs::write(
            wf_dir.join("zebra.md"),
            "---\nname: zebra\ndescription: Z\n---\nPrep zebra.",
        )
        .unwrap();
        std::fs::write(
            wf_dir.join("alpha.md"),
            "---\nname: alpha\ndescription: A\n---\nPrep alpha.",
        )
        .unwrap();

        let wfs = load_workflows(&dir);
        assert_eq!(wfs.len(), 2);
        assert_eq!(wfs[0].name, "alpha");
        assert_eq!(wfs[1].name, "zebra");
        assert_eq!(prompt_for(&wfs[0], "the api"), "Prep alpha.\n\nGoal: the api");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn workflow_without_body_gets_default_directive() {
        let dir = std::env::temp_dir().join(format!("ai-wf-empt-{}", std::process::id()));
        let wf_dir = dir.join(".ai").join("workflows");
        std::fs::create_dir_all(&wf_dir).unwrap();
        std::fs::write(
            wf_dir.join("blank.md"),
            "---\nname: blank\ndescription: No body\n---\n",
        )
        .unwrap();

        let wfs = load_workflows(&dir);
        assert_eq!(wfs.len(), 1);
        let p = prompt_for(&wfs[0], "share");
        assert!(p.contains("`blank` workflow"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_workflows_dir_returns_empty() {
        let dir = std::env::temp_dir().join(format!("ai-wf-empty-{}", std::process::id()));
        assert!(load_workflows(&dir).is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn workflow_allowed_tools_parsed() {
        let dir = std::env::temp_dir().join(format!("ai-wf-tools-{}", std::process::id()));
        let wf_dir = dir.join(".ai").join("workflows");
        std::fs::create_dir_all(&wf_dir).unwrap();
        std::fs::write(
            wf_dir.join("build.md"),
            "---\nname: build\ndescription: b\nallowedTools: run_tests, edit_file\n---\nBuild.",
        )
        .unwrap();

        let wfs = load_workflows(&dir);
        assert_eq!(wfs.len(), 1);
        let expected: Vec<String> = vec!["run_tests".into(), "edit_file".into()];
        assert_eq!(wfs[0].allowed_tools, expected);
        std::fs::remove_dir_all(&dir).ok();
    }
}