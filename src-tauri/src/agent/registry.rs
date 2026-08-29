//! Canonical tool metadata — the single source of truth for every tool.
//!
//! Before this module existed, three independent hand-maintained string lists
//! described each tool and could silently drift:
//!
//!   1. `core::tool_schemas()`          — what the model is told exists
//!   2. `policy::default_allow()`       — read-only/safe tools that skip approval
//!   3. `subagent::{READ_ONLY_TOOLS, IMPLEMENT_TOOLS, CHILD_NEVER}` — what
//!      delegated subagents may call
//!
//! Concretely, `analyze_bug`, `review_code` and `browse_web` were dispatched by
//! `tools::dispatch` (and listed by `ToolCall::name`) but never registered in
//! `tool_schemas`, so the model could never call them in agent mode.
//!
//! This module keeps one row per tool, keyed by the exact snake_case name that
//! `ToolCall::name()` produces, with two independent attributes:
//!
//!   * `delegation` — how a delegated subagent may use the tool
//!     ([`SubagentAccess`]); parent agents may always call anything.
//!   * `read_only`  — the tool is safe to auto-allow (skips the approval gate
//!     unless a policy rule overrides it), returned by [`is_read_only`].
//!
//! Every consumer that used to hard-code a string list now derives it from here,
//! and [`validate()`] (called by a unit test) proves the registry names exactly
//! match the dispatcher's `ToolCall::name()` output so the two can never drift
//! again.

/// How a delegated (subagent) context may use a tool.
///
/// Order matters for precedence: `ParentOnly` must always win over the others
/// (a child may never use a parent-only tool even if it also matches another
/// category), and `Implement` is additive on top of `ReadOnly`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum SubagentAccess {
    /// Available to read-only profiles (`explore`, `review`) **and** to the
    /// `implement` profile — the base set every child gets.
    ReadOnly,
    /// Available only to the `implement` profile, on top of `ReadOnly`.
    Implement,
    /// Reserved for the parent agent; no child may call it (`CHILD_NEVER`).
    ParentOnly,
    /// Not exposed to any child context at all.
    Forbidden,
}

/// One row of the canonical tool table.
#[derive(Debug, Clone, Copy)]
pub struct ToolMeta {
    /// Exact snake_case name emitted by `ToolCall::name()`.
    pub name: &'static str,
    /// Safe to auto-allow (skips the approval gate) unless overridden.
    pub read_only: bool,
    /// How subagents may use the tool.
    pub delegation: SubagentAccess,
}

/// Build one entry concisely.
const fn rw(name: &'static str) -> ToolMeta {
    ToolMeta {
        name,
        read_only: true,
        delegation: SubagentAccess::ReadOnly,
    }
}

const fn ro_impl(name: &'static str) -> ToolMeta {
    ToolMeta {
        name,
        read_only: true,
        delegation: SubagentAccess::Implement,
    }
}

const fn problem(name: &'static str) -> ToolMeta {
    ToolMeta {
        name,
        read_only: true,
        delegation: SubagentAccess::Forbidden,
    }
}

const fn parent(name: &'static str) -> ToolMeta {
    ToolMeta {
        name,
        read_only: true,
        delegation: SubagentAccess::ParentOnly,
    }
}

const fn mutating(name: &'static str, delegation: SubagentAccess) -> ToolMeta {
    ToolMeta {
        name,
        read_only: false,
        delegation,
    }
}

/// The canonical tool table. Keep this in sync with `ToolCall::name()` —
/// [`validate()`] / [`super::core::test_registry_matches_dispatcher`] enforce it.
pub static TOOLS: &[ToolMeta] = &[
    // ---- read-only / safe (default-allow) tools --------------------------
    rw("glob_search_codebase"),
    rw("search_file_contents"),
    rw("semantic_search_codebase"),
    rw("view_file_structure"),
    rw("read_file_range"),
    rw("read_file_chars"),
    rw("list_dir"),
    rw("read_skill"),
    rw("read_plan"),
    rw("get_scratchpad_folder"),
    rw("git_status"),
    rw("git_diff"),
    rw("git_blame"),
    rw("git_pr_status"),
    rw("git_ci_status"),
    rw("summarize_changes"),
    rw("read_lints"),
    rw("view_repo_map"),
    rw("suggest_skills"),
    // `calculate` is deterministic arithmetic — safe, but deliberately NOT in
    // the default-allow set (it was historically excluded); keep as-is.
    mutating("calculate", SubagentAccess::ReadOnly),
    rw("attach_file"),
    rw("search_attached_files"),
    rw("tree_sitter_query"),
    // Human-interaction tools: the user is always in the loop, so they are
    // default-allow on the parent; children can never reach them (ParentOnly).
    parent("ask_question"),
    parent("send_to_user"),
    rw("run_tests"),
    rw("get_todo_list"),
    rw("set_todo_list"),
    rw("mark_todo_item_done"),
    rw("list_mcp_servers"),
    rw("detach_file"),
    // First-class subagents (P1-8): delegation itself is routine on the
    // parent; children obviously cannot delegate further.
    parent("task"),

    // ---- mutating / build tools ------------------------------------------
    mutating("apply_file_diff", SubagentAccess::Implement),
    mutating("write_file", SubagentAccess::Implement),
    mutating("create_folder", SubagentAccess::Implement),
    mutating("copy_file_or_folder", SubagentAccess::Implement),
    mutating("move_file_or_folder", SubagentAccess::Implement),
    mutating("delete_file_or_folder", SubagentAccess::ParentOnly),
    mutating("execute_terminal_command", SubagentAccess::Implement),
    mutating("run_python", SubagentAccess::Implement),
    mutating("run_javascript", SubagentAccess::Implement),
    mutating("call_mcp_tool", SubagentAccess::Implement),
    mutating("add_mcp_server", SubagentAccess::ParentOnly),
    mutating("remove_mcp_server", SubagentAccess::ParentOnly),
    mutating("download_file", SubagentAccess::ParentOnly),

    // ---- git -----------
    mutating("git_commit", SubagentAccess::ParentOnly),
    mutating("git_checkpoint", SubagentAccess::ParentOnly),
    mutating("git_revert", SubagentAccess::ParentOnly),
    mutating("git_push", SubagentAccess::Forbidden),
    mutating("git_pull", SubagentAccess::Forbidden),
    mutating("git_create_branch", SubagentAccess::Forbidden),
    mutating("create_pr", SubagentAccess::Forbidden),

    // ---- plans / skills (parent-owned lifecycle) --------------------------
    mutating("create_plan", SubagentAccess::Forbidden),
    mutating("update_plan", SubagentAccess::Forbidden),
    // execute_plan is intercepted by the orchestrator — parent only.
    mutating("execute_plan", SubagentAccess::ParentOnly),
    mutating("create_skill", SubagentAccess::Forbidden),

    // ---- capabilities new to this tool set (were MISSING from the schema) --
    // These were dispatched but had no `tool_schemas` entry, so the model
    // could never see them in agent mode. Now registered (with the schema
    // added in `core::tool_schemas`) so they are both discoverable and
    // correctly classified.
    problem("analyze_bug"),
    problem("review_code"),
    problem("browse_web"),

    // ---- everything else (Forbidden for children by omission) ------------
    mutating("transcribe_audio", SubagentAccess::Forbidden),
    mutating("web_search", SubagentAccess::Forbidden),
    mutating("web_extract", SubagentAccess::Forbidden),
];

impl ToolMeta {
    pub fn get(name: &str) -> Option<&'static ToolMeta> {
        TOOLS.iter().find(|m| m.name == name)
    }

    /// Parent agents may always call every registered tool.
    pub fn parent_allowed(&self) -> bool {
        true
    }
}

/// Is this tool safe to auto-allow (skip the approval gate) by default?
pub fn is_read_only(name: &str) -> bool {
    ToolMeta::get(name).map(|m| m.read_only).unwrap_or(false)
}

/// The canonical set of every registered tool name (used by the drift guard).
pub fn all_names() -> Vec<&'static str> {
    TOOLS.iter().map(|m| m.name).collect()
}

/// Tool names a delegated subagent of `profile` may call.
///
/// Derived straight from the registry so `tool_allowed` can never disagree
/// with the metadata. `profile` is lowercased by the caller.
pub fn subagent_allowed(profile: &str) -> Vec<&'static str> {
    let profile = profile.trim().to_ascii_lowercase();
    let implement = matches!(profile.as_str(), "implement");
    TOOLS
        .iter()
        .filter(|m| match (m.delegation, implement) {
            // ParentOnly always wins; Forbidden is never callable by children.
            (SubagentAccess::ParentOnly, _) => false,
            (SubagentAccess::Forbidden, _) => false,
            // ReadOnly is the base set for read-only profiles AND the
            // implement profile (which adds Implement on top).
            (SubagentAccess::ReadOnly, _) => true,
            (SubagentAccess::Implement, true) => true,
            (SubagentAccess::Implement, false) => false,
        })
        .map(|m| m.name)
        .collect()
}

/// Is this tool reserved for the parent agent (never callable by a child)?
pub fn is_parent_only(name: &str) -> bool {
    ToolMeta::get(name)
        .map(|m| m.delegation == SubagentAccess::ParentOnly)
        .unwrap_or(false)
}

/// Names in the parent-only set (`CHILD_NEVER`) — used by tests.
pub fn parent_only_names() -> Vec<&'static str> {
    TOOLS
        .iter()
        .filter(|m| m.delegation == SubagentAccess::ParentOnly)
        .map(|m| m.name)
        .collect()
}

/// Prove the registry is internally consistent and covers every name produced
/// by the dispatcher. A unit test wires the dispatcher's names in.
pub fn validate(dispatcher_names: &[&str]) -> Result<(), String> {
    let mut registered: Vec<&'static str> = all_names();
    registered.sort_unstable();
    let mut dispatched: Vec<&str> = dispatcher_names.to_vec();
    dispatched.sort_unstable();
    dispatched.dedup();

    let missing_from_registry: Vec<&str> = dispatched
        .iter()
        .filter(|n| !registered.iter().any(|r| r == *n))
        .cloned()
        .collect();
    if !missing_from_registry.is_empty() {
        return Err(format!(
            "tools dispatched but missing from registry: {}",
            missing_from_registry.join(", ")
        ));
    }

    let extra_in_registry: Vec<&str> = registered
        .iter()
        .filter(|r| !dispatched.iter().any(|d| d == *r))
        .map(|r| *r)
        .collect();
    if !extra_in_registry.is_empty() {
        return Err(format!(
            "registry names with no dispatcher arm: {}",
            extra_in_registry.join(", ")
        ));
    }

    for meta in TOOLS {
        if meta.name.trim().is_empty() {
            return Err("registry contains an empty tool name".into());
        }
    }
    Ok(())
}
