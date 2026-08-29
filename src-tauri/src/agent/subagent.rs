//! P1-8 first-class subagents.
//!
//! A subagent is a named profile (`explore`, `implement`, `review`) that runs
//! its own focused tool loop on a spare engine worker and reports distilled
//! findings back to the parent agent as one tool observation. Profiles differ
//! in mission, step budget, and tool guidance — the parent's context stays
//! clean instead of absorbing raw file dumps.
//!
//! Enforcement layers:
//!   * **Restricted child permissions** — [`child_verdict`] hard-denies tools
//!     a child may never call, per profile, inside `policy::check`. Guidance
//!     in the child system prompt mirrors these lists, but enforcement is
//!     centralized and cannot be talked around.
//!   * **Depth guard** — children run with a thread-local depth counter;
//!     nesting beyond [`MAX_SUBAGENT_DEPTH`] is refused.
//!   * **Occupancy leasing** — the orchestrator leases spare engine workers
//!     (`ToolState.leased_workers`) so a child can never collide with the
//!     primary loop or another running child.

use std::borrow::Cow;
use std::cell::{Cell, RefCell};
use std::path::Path;

/// A reusable specialist the parent agent can delegate focused work to.
pub struct SubagentProfile {
    pub name: Cow<'static, str>,
    /// One-line capability summary surfaced to the parent model.
    pub description: Cow<'static, str>,
    /// System instructions for the child loop.
    pub system_prompt: Cow<'static, str>,
    /// Hard cap on tool-call rounds for the child (keeps runaway children
    /// from eating the whole budget).
    pub max_steps: usize,
}

pub const PROFILES: &[SubagentProfile] = &[
    SubagentProfile {
        name: Cow::Borrowed("explore"),
        description: Cow::Borrowed("Read-only codebase reconnaissance: find where things live, map call paths, summarize architecture."),
        system_prompt: Cow::Borrowed("You are an EXPLORATION subagent. Mission: investigate the codebase and answer the parent's task with precise, citable findings.\n\
            Use ONLY read tools (glob_search_codebase, search_file_contents, view_file_structure, read_file_range, read_file_chars, list_dir).\n\
            Do NOT modify files or run shell commands. Cite exact `path:line` references. Finish with a compact findings report; omit raw file dumps."),
        max_steps: 4,
    },
    SubagentProfile {
        name: Cow::Borrowed("implement"),
        description: Cow::Borrowed("Focused implementation of one well-scoped change: edit code, then verify it compiles/tests."),
        system_prompt: Cow::Borrowed("You are an IMPLEMENTATION subagent. Mission: complete exactly ONE well-scoped change end-to-end.\n\
            Read before writing; keep edits minimal and idiomatic; run tests/typecheck after editing when possible.\n\
            Do NOT start unrelated work. Finish with a short report: what changed (paths), how it was verified, any follow-ups."),
        max_steps: 6,
    },
    SubagentProfile {
        name: Cow::Borrowed("review"),
        description: Cow::Borrowed("Adversarial review of recent changes or specified files: bugs, edge cases, style violations."),
        system_prompt: Cow::Borrowed("You are a CODE REVIEW subagent. Mission: scrutinize the specified changes/files for correctness bugs, missing edge cases, security issues, and convention violations.\n\
            Use ONLY read tools plus git_diff/git_status for context. Do NOT modify anything.\n\
            Report findings ordered by severity with `path:line` references; say explicitly when something is fine."),
        max_steps: 4,
    },
];

/// Resolve a profile by name (case-insensitive).
pub fn lookup(name: &str) -> Option<&'static SubagentProfile> {
    let lower = name.trim().to_ascii_lowercase();
    PROFILES.iter().find(|p| p.name.as_ref() == lower)
}

/// Comma-separated profile list for the parent system prompt.
pub fn catalog() -> String {
    PROFILES
        .iter()
        .map(|p| format!("- `{}`: {}", p.name, p.description))
        .collect::<Vec<_>>()
        .join("\n")
}

// ---------------------------------------------------------------------------
// Depth guard + child context (thread-local).
//
// Each subagent loop runs on its own native thread, so a thread-local depth
// counter gives exact per-context tracking with zero contention: the parent's
// worker thread stays at depth 0 while every child thread starts at 1. The
// parent's parallel read-only fan-out on its own thread is never miscounted.
// ---------------------------------------------------------------------------

/// Maximum nesting: parent (0) → child (1) → grandchild (2). Deeper `task`
/// calls are refused.
pub const MAX_SUBAGENT_DEPTH: usize = 2;

thread_local! {
    static DEPTH: Cell<usize> = const { Cell::new(0) };
    static PROFILE: RefCell<Option<String>> = const { RefCell::new(None) };
    static CUSTOM_MODES: RefCell<Vec<CustomModeData>> = const { RefCell::new(Vec::new()) };
}

/// RAII guard: marks the current thread as running inside a subagent of
/// `profile` at the next depth level. Restores the previous state on drop.
pub struct ChildGuard {
    prev_depth: usize,
    prev_profile: Option<String>,
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        DEPTH.with(|d| d.set(self.prev_depth));
        PROFILE.with(|p| *p.borrow_mut() = self.prev_profile.clone());
    }
}

/// Try to enter a subagent context on the current thread. Fails when the
/// nesting would exceed [`MAX_SUBAGENT_DEPTH`].
pub fn enter_child(profile: &'static SubagentProfile) -> Result<ChildGuard, String> {
    let prev_depth = DEPTH.with(Cell::get);
    if prev_depth >= MAX_SUBAGENT_DEPTH {
        return Err(format!(
            "Subagent nesting too deep (max {MAX_SUBAGENT_DEPTH}). \
             Subagents must not spawn further subagents beyond this limit."
        ));
    }
    let prev_profile = PROFILE.with(|p| p.borrow_mut().take());
    DEPTH.with(|d| d.set(prev_depth + 1));
    PROFILE.with(|p| *p.borrow_mut() = Some(profile.name.to_string()));
    Ok(ChildGuard {
        prev_depth,
        prev_profile,
    })
}

/// Current subagent nesting depth on this thread (0 = primary agent loop).
pub fn current_depth() -> usize {
    DEPTH.with(Cell::get)
}

/// Profile running on this thread, if any.
pub fn current_profile() -> Option<String> {
    PROFILE.with(|p| p.borrow().clone())
}

// ---------------------------------------------------------------------------
// Restricted child permissions.
//
// Hard tool restrictions per profile, enforced in `policy::check` via
// [`child_verdict`]. Children additionally inherit ALL normal policy gates
// (red zone, workspace scoping, ask/allow/deny, decision memory).
//
// The per-tool rules live in the canonical [`super::registry`] table so the
// subagent allow-lists can never drift from the dispatcher or the schema set.
// ---------------------------------------------------------------------------

/// Is `tool` allowed for `profile`? Read-only profiles get exactly the
/// read-only set; `implement` adds mutating/build tools. Parent-only tools are
/// off-limits to every child; custom modes additionally inherit the
/// parent-only restriction on top of their declared `allowedTools`.
pub fn tool_allowed(profile: &str, tool: &str) -> bool {
    if super::registry::is_parent_only(tool) {
        return false;
    }
    let lower = profile.trim().to_ascii_lowercase();
    match lower.as_str() {
        "explore" | "review" | "implement" => {
            super::registry::subagent_allowed(&lower).contains(&tool)
        }
        _ => check_custom_mode_tools(profile, tool),
    }
}

/// Parent-only tool names (previously `CHILD_NEVER`) — never callable by a
/// child, regardless of profile or custom-mode allow list.
pub fn child_never() -> Vec<&'static str> {
    super::registry::parent_only_names()
}

/// Policy hook: when called from INSIDE a subagent loop (depth ≥ 1), deny
/// tools the running profile may not use. Returns `None` when no restriction
/// applies (parent context or allowed call) so the normal gates proceed.
pub fn child_verdict(tool: &str) -> Option<String> {
    if current_depth() == 0 {
        return None;
    }
    let profile = current_profile()?;
    if tool_allowed(&profile, tool) {
        None
    } else {
        Some(format!(
            "Tool `{tool}` is not available to `{profile}` subagents. \
             Finish your focused task and report back instead."
        ))
    }
}

/// Compact markdown catalog of the tools available to a child, built from the
/// canonical JSON schemas so the docs can never drift from the dispatcher.
/// Only lists tools the profile is actually allowed to call.
pub fn tool_catalog_markdown(profile_name: &str) -> String {
    let schemas = super::core::tool_schemas();
    let mut lines: Vec<String> = Vec::new();
    // Deterministic order: schema map order is arbitrary; sort by name.
    let mut names: Vec<&str> = schemas
        .keys()
        .copied()
        .filter(|n| tool_allowed(profile_name, n))
        .collect();
    names.sort_unstable();
    for name in names {
        let mut line = format!("- `{name}`:");
        if let Some(schema) = schemas.get(name) {
            let props = schema
                .get("properties")
                .and_then(|p| p.as_object())
                .cloned()
                .unwrap_or_default();
            let required: Vec<&str> = schema
                .get("required")
                .and_then(|r| r.as_array())
                .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
                .unwrap_or_default();
            let mut parts: Vec<String> = Vec::new();
            for (pname, pschema) in &props {
                let ty = pschema
                    .get("type")
                    .and_then(|t| t.as_str())
                    .unwrap_or("any");
                let req = if required.contains(&pname.as_str()) {
                    ""
                } else {
                    "?"
                };
                parts.push(format!("{pname}{req}({ty})"));
            }
            line.push_str(&format!(" {}", parts.join(", ")));
        }
        lines.push(line);
    }
    lines.join("\n")
}

// ---------------------------------------------------------------------------
// User-defined custom modes (`.ai/modes/*.md`).
// ---------------------------------------------------------------------------

/// A user-defined agent mode loaded from a `.ai/modes/*.md` file.
#[derive(Debug, Clone)]
pub struct Mode {
    pub name: String,
    pub description: String,
    pub system_prompt: String,
    pub allowed_tools: Vec<String>,
    pub allowed_globs: Option<Vec<String>>,
    pub model_override: Option<String>,
}

/// Internal storage for custom mode tool lists, accessed via thread-local.
struct CustomModeData {
    name: String,
    allowed_tools: Vec<String>,
}

/// Check if a tool is allowed for a custom mode (not in the built-in profiles).
fn check_custom_mode_tools(profile: &str, tool: &str) -> bool {
    CUSTOM_MODES.with(|modes| {
        modes
            .borrow()
            .iter()
            .any(|m| m.name == profile && m.allowed_tools.iter().any(|t| t == tool))
    })
}

/// Load custom agent modes from `.ai/modes/*.md` files in the given workspace.
///
/// File format mirrors skills.rs frontmatter:
/// ```markdown
/// ---
/// name: custom-mode
/// description: A custom agent mode
/// allowedTools: read_file_chars, glob_search_codebase
/// globs: "*.rs"
/// model: some-model
/// ---
/// # Custom Mode
/// You are a custom mode. Do specialized work.
/// ```
pub fn load_modes(workspace: &Path) -> Vec<Mode> {
    let modes_dir = workspace.join(".ai").join("modes");
    if !modes_dir.is_dir() {
        return Vec::new();
    }
    let mut modes: Vec<Mode> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&modes_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() && path.extension().map(|e| e == "md").unwrap_or(false) {
                if let Some(mode) = parse_mode_file(&path) {
                    modes.push(mode);
                }
            }
        }
    }
    modes.sort_by(|a, b| a.name.cmp(&b.name));
    modes
}

/// Register loaded custom modes into the thread-local registry so
/// [`tool_allowed`] and [`child_verdict`] can enforce their restrictions.
pub fn register_modes(modes: &[Mode]) {
    CUSTOM_MODES.with(|m| {
        *m.borrow_mut() = modes
            .iter()
            .map(|m| CustomModeData {
                name: m.name.clone(),
                allowed_tools: m.allowed_tools.clone(),
            })
            .collect();
    });
}

/// Build a [`SubagentProfile`] from a user-defined [`Mode`].
///
/// The profile is heap-allocated and leaked so it has `'static` lifetime,
/// matching the signature expected by [`enter_child`]. The mode's allowed
/// tools are enforced via the thread-local custom mode registry (see
/// [`register_modes`]).
pub fn build_mode_profile(mode: &Mode) -> &'static SubagentProfile {
    let name: &'static str = Box::leak(mode.name.clone().into_boxed_str());
    let description: &'static str = Box::leak(mode.description.clone().into_boxed_str());
    let system_prompt: &'static str = if mode.system_prompt.is_empty() {
        Box::leak(
            format!(
                "You are a `{}` subagent. Complete your assigned task and report back with findings.",
                mode.name
            )
            .into_boxed_str(),
        )
    } else {
        Box::leak(mode.system_prompt.clone().into_boxed_str())
    };
    Box::leak(Box::new(SubagentProfile {
        name: Cow::Borrowed(name),
        description: Cow::Borrowed(description),
        system_prompt: Cow::Borrowed(system_prompt),
        max_steps: 8,
    }))
}

/// Parse a single `.ai/modes/*.md` file into a [`Mode`].
/// Returns `None` on missing file, I/O error, or missing required fields.
fn parse_mode_file(path: &Path) -> Option<Mode> {
    let text = std::fs::read_to_string(path).ok()?;
    let (front, body) = split_frontmatter(&text)?;
    let name = extract_yaml_field(&front, "name")?;
    let description = extract_yaml_field(&front, "description").unwrap_or_default();
    let allowed_tools = extract_yaml_list(&front, "allowedTools");
    let allowed_globs = extract_yaml_list_opt(&front, "globs");
    let model_override = extract_yaml_field(&front, "model");
    Some(Mode {
        name,
        description,
        system_prompt: body.trim().to_string(),
        allowed_tools,
        allowed_globs,
        model_override,
    })
}

/// Split `---\nkey: value\n---\nbody` frontmatter.
/// Returns `(frontmatter_text, body_text)` or `None` if missing.
fn split_frontmatter(text: &str) -> Option<(String, String)> {
    let rest = text.strip_prefix("---")?;
    let end = rest.find("\n---")?;
    let front = rest[..end].to_string();
    let body = rest[end + 4..].trim_start().to_string();
    Some((front, body))
}

/// Extract a scalar YAML field value from the frontmatter text.
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

/// Extract a YAML list field as `Vec<String>` from the frontmatter text.
/// Supports comma-separated values on a single line.
fn extract_yaml_list(frontmatter: &str, key: &str) -> Vec<String> {
    extract_yaml_list_opt(frontmatter, key).unwrap_or_default()
}

/// Extract a YAML list field as `Option<Vec<String>>`.
fn extract_yaml_list_opt(frontmatter: &str, key: &str) -> Option<Vec<String>> {
    let val = extract_yaml_field(frontmatter, key)?;
    if val.is_empty() {
        return Some(Vec::new());
    }
    let items: Vec<String> = val
        .split(',')
        .map(|s| s.trim().to_string())
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
    fn lookup_is_case_insensitive_and_total_over_catalog() {
        assert_eq!(lookup("EXPLORE").unwrap().name.as_ref(), "explore");
        assert!(lookup("  implement ").is_some());
        assert!(lookup("review").is_some());
        assert!(lookup("nonexistent").is_none());
        // Every catalog entry resolves (guards typo'd additions).
        for p in PROFILES {
            assert!(lookup(p.name.as_ref()).is_some());
        }
    }

    #[test]
    fn budgets_are_sane() {
        for p in PROFILES {
            assert!((1..=12).contains(&p.max_steps), "{} budget", p.name);
            assert!(!p.system_prompt.is_empty());
            assert!(!p.description.is_empty());
        }
    }

    #[test]
    fn catalog_lists_every_profile() {
        let c = catalog();
        for p in PROFILES {
            assert!(c.contains(&format!("`{}`", p.name)));
        }
    }

    #[test]
    fn child_guard_tracks_depth_and_restores_on_drop() {
        assert_eq!(current_depth(), 0);
        {
            let g = enter_child(lookup("explore").unwrap()).unwrap();
            assert_eq!(current_depth(), 1);
            assert_eq!(current_profile(), Some("explore".to_string()));
            drop(g);
        }
        assert_eq!(current_depth(), 0);
        assert_eq!(current_profile(), None);
    }

    #[test]
    fn nesting_beyond_max_is_refused() {
        let _g1 = enter_child(lookup("implement").unwrap()).unwrap();
        let _g2 = enter_child(lookup("explore").unwrap()).unwrap();
        assert_eq!(current_depth(), MAX_SUBAGENT_DEPTH);
        assert!(enter_child(lookup("review").unwrap()).is_err());
    }

    #[test]
    fn read_only_profiles_cannot_mutate() {
        for profile in ["explore", "review"] {
            assert!(tool_allowed(profile, "read_file_range"));
            assert!(tool_allowed(profile, "git_diff"));
            assert!(!tool_allowed(profile, "write_file"), "{profile}");
            assert!(
                !tool_allowed(profile, "execute_terminal_command"),
                "{profile}"
            );
        }
        assert!(tool_allowed("implement", "apply_file_diff"));
        assert!(tool_allowed("implement", "run_tests"));
        assert!(tool_allowed("implement", "execute_terminal_command"));
    }

    #[test]
    fn no_child_may_delegate_or_commit_or_delete() {
        for profile in ["explore", "implement", "review"] {
            for tool in CHILD_NEVER {
                assert!(
                    !tool_allowed(profile, tool),
                    "{profile} must not call {tool}"
                );
            }
        }
    }

    #[test]
    fn unknown_profile_allows_nothing() {
        assert!(!tool_allowed("mystery", "list_dir"));
    }

    #[test]
    fn child_verdict_only_applies_at_depth() {
        // Parent context: unrestricted (None = defer to normal gates).
        assert!(child_verdict("write_file").is_none());

        let _g = enter_child(lookup("explore").unwrap()).unwrap();
        assert_eq!(
            child_verdict("write_file").as_deref(),
            Some(
                "Tool `write_file` is not available to `explore` subagents. \
             Finish your focused task and report back instead."
            )
        );
        assert!(child_verdict("read_file_range").is_none());
        assert!(child_verdict("task").is_some());
        drop(_g);

        // Implement children may write.
        let _g = enter_child(lookup("implement").unwrap()).unwrap();
        assert!(child_verdict("write_file").is_none());
        assert!(child_verdict("git_revert").is_some());
    }

    #[test]
    fn tool_catalog_lists_only_allowed_tools_with_params() {
        let explore = tool_catalog_markdown("explore");
        assert!(explore.contains("`glob_search_codebase`: pattern(string)"));
        assert!(!explore.contains("write_file"));
        assert!(!explore.contains("task"));

        let implement = tool_catalog_markdown("implement");
        assert!(implement.contains("`apply_file_diff`"));
        assert!(implement.contains("`run_tests`"));
        // Optional params are marked with `?`.
        assert!(implement.contains("timeout_secs?(integer)"));

        let review = tool_catalog_markdown("review");
        assert!(review.contains("`git_status`"));
        assert!(!review.contains("`call_mcp_tool`"));
    }

    #[test]
    fn load_modes_returns_sorted_by_name() {
        let dir = std::env::temp_dir().join(format!("ai-modes-sort-{}", std::process::id()));
        let modes_dir = dir.join(".ai").join("modes");
        std::fs::create_dir_all(&modes_dir).unwrap();
        std::fs::write(
            modes_dir.join("zebra.md"),
            "---\nname: zebra\ndescription: Z\n---\nZebra body",
        )
        .unwrap();
        std::fs::write(
            modes_dir.join("alpha.md"),
            "---\nname: alpha\ndescription: A\n---\nAlpha body",
        )
        .unwrap();

        let modes = load_modes(&dir);
        assert_eq!(modes.len(), 2);
        assert_eq!(modes[0].name, "alpha");
        assert_eq!(modes[1].name, "zebra");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn parse_mode_file_extracts_all_fields() {
        let dir = std::env::temp_dir().join(format!("ai-modes-parse-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.md");
        std::fs::write(
            &path,
            "---\nname: my-mode\ndescription: A test mode\nallowedTools: read_file_chars, glob_search_codebase\nglobs: \"*.rs, *.ts\"\nmodel: my-model\n---\nDo the thing.\n",
        )
        .unwrap();

        let mode = parse_mode_file(&path).unwrap();
        assert_eq!(mode.name, "my-mode");
        assert_eq!(mode.description, "A test mode");
        assert_eq!(mode.system_prompt, "Do the thing.");
        assert_eq!(mode.allowed_tools, vec!["read_file_chars", "glob_search_codebase"]);
        assert_eq!(mode.allowed_globs, Some(vec!["*.rs".into(), "*.ts".into()]));
        assert_eq!(mode.model_override.as_deref(), Some("my-model"));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn build_mode_profile_produces_static_ref() {
        let mode = Mode {
            name: "custom-test".into(),
            description: "Custom test mode".into(),
            system_prompt: "Be helpful.".into(),
            allowed_tools: vec!["read_file_range".into(), "glob_search_codebase".into()],
            allowed_globs: None,
            model_override: None,
        };
        let profile = build_mode_profile(&mode);
        assert_eq!(profile.name.as_ref(), "custom-test");
        assert_eq!(profile.description.as_ref(), "Custom test mode");
        assert_eq!(profile.system_prompt.as_ref(), "Be helpful.");
        assert_eq!(profile.max_steps, 8);
    }

    #[test]
    fn custom_mode_tools_enforced_through_tool_allowed() {
        let dir = std::env::temp_dir().join(format!("ai-modes-tools-{}", std::process::id()));
        let modes_dir = dir.join(".ai").join("modes");
        std::fs::create_dir_all(&modes_dir).unwrap();
        std::fs::write(
            modes_dir.join("reader.md"),
            "---\nname: reader\ndescription: Read only\nallowedTools: read_file_range, glob_search_codebase, list_dir\n---\nRead stuff.",
        )
        .unwrap();

        let modes = load_modes(&dir);
        register_modes(&modes);
        assert!(tool_allowed("reader", "read_file_range"));
        assert!(tool_allowed("reader", "glob_search_codebase"));
        assert!(tool_allowed("reader", "list_dir"));
        assert!(!tool_allowed("reader", "write_file"));
        assert!(!tool_allowed("reader", "execute_terminal_command"));

        // CHILD_NEVER still applies.
        assert!(!tool_allowed("reader", "task"));
        assert!(!tool_allowed("reader", "git_commit"));

        // Clear for other tests.
        register_modes(&[]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_modes_dir_returns_empty() {
        let dir = std::env::temp_dir().join(format!("ai-modes-empty-{}", std::process::id()));
        let modes = load_modes(&dir);
        assert!(modes.is_empty());
    }
}
