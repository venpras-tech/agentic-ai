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

use super::ToolCall;

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
    SubagentProfile {
        name: Cow::Borrowed("debug"),
        description: Cow::Borrowed("Reproduce and diagnose a failure with evidence: run tests/commands, read logs and code, then attribute root cause."),
        system_prompt: Cow::Borrowed("You are a DEBUG subagent. Mission: turn one reported failure into a diagnosed root cause backed by evidence.\n\
            Work in phases using exactly the tools you need: state an explicit hypothesis; reproduce it with run_tests or a focused execute_terminal_command and capture the actual logs/errors; read the relevant source ranges to trace the failing path; confirm or revise the hypothesis; then report the root cause with precise path:line evidence, the minimal repro, and a suggested fix (do NOT edit files unless the parent explicitly asked you to fix). Do not flail — gather evidence before concluding, and clearly distinguish observed facts from speculation."),
        max_steps: 6,
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
    if let Some(builtin) = builtin_mode_tools(profile) {
        return builtin.iter().any(|t| *t == tool);
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
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Mode {
    pub name: String,
    pub description: String,
    pub system_prompt: String,
    pub allowed_tools: Vec<String>,
    pub allowed_globs: Option<Vec<String>>,
    pub model_override: Option<String>,
}

/// Internal storage for custom mode tool / glob lists, accessed via
/// thread-local. Globs are enforced against file-mutating tool targets.
struct CustomModeData {
    name: String,
    allowed_tools: Vec<String>,
    allowed_globs: Option<Vec<String>>,
    model_override: Option<String>,
}

/// Check if a tool is allowed for a custom mode (not in the built-in profiles).
/// A mode declared without `allowedTools:` is unrestricted (all tools pass); an
/// unknown mode name is denied conservatively.
fn check_custom_mode_tools(profile: &str, tool: &str) -> bool {
    CUSTOM_MODES.with(|modes| {
        let modes = modes.borrow();
        match modes.iter().find(|m| m.name == profile) {
            None => false,
            Some(m) => m.allowed_tools.is_empty() || m.allowed_tools.iter().any(|t| t == tool),
        }
    })
}

/// Built-in first-class modes. These are served alongside user-defined modes
/// (`.ai/modes/*.md`) and enforced by [`tool_allowed`] just like a custom mode.
///
/// - `ask` — read-only recon: the agent may inspect the codebase and answer,
///   but may not mutate files or run mutating commands.
/// - `debug` — read-only recon plus test execution and a focused terminal
///   workflow, so the agent can reproduce and diagnose a failure.
pub fn builtin_mode_tools(profile: &str) -> Option<Vec<&'static str>> {
    let lower = profile.trim().to_ascii_lowercase();
    match lower.as_str() {
        "ask" => Some(super::registry::subagent_allowed("explore")),
        "debug" => Some({
            let mut tools = super::registry::subagent_allowed("explore");
            for extra in [
                "run_tests",
                "execute_terminal_command",
                "read_file_range",
                "list_directory",
            ] {
                if !tools.iter().any(|t| *t == extra) {
                    tools.push(extra);
                }
            }
            tools
        }),
        "edit" => Some({
            let mut tools = super::registry::subagent_allowed("explore");
            for extra in [
                "write_file",
                "apply_file_diff",
                "create_folder",
                "copy_file_or_folder",
                "move_file_or_folder",
                "run_tests",
                "read_file_range",
                "list_directory",
            ] {
                if !tools.iter().any(|t| *t == extra) {
                    tools.push(extra);
                }
            }
            tools
        }),
        _ => None,
    }
}

/// The `System` prompt fragment for a built-in mode (appended after the agent
/// system prompt, mirroring user-defined modes' `# Active mode` section).
pub fn builtin_mode_system_prompt(profile: &str) -> Option<&'static str> {
    match profile.trim().to_ascii_lowercase().as_str() {
        "ask" => Some(
            "## Active mode: ask\n\
             You are in READ-ONLY mode. Investigate the codebase, answer the \
             user's question, and propose changes — but do NOT modify any files \
             and do NOT run mutating commands. Call reasoning/analysis tools only.",
        ),
        "debug" => Some(
            "## Active mode: debug\n\
             You are in DEBUG mode. Reproduce the reported failure with logs or \
             the test suite, diagnose the root cause using evidence, then propose \
             a fix. Only introduce edits that are clearly part of a verified fix.",
        ),
        "edit" => Some(
            "## Active mode: edit\n\
             You are in CODE-EDIT-ONLY mode. You may read the codebase, propose \
             and apply file edits (write, diff-apply, copy, move, create folders), \
             and run read-only checks (tests, linting). You must NOT run arbitrary \
             shell commands, commit/push to git, or perform any destructive \
             operations. Focus on making clean, targeted code changes.",
        ),
        _ => None,
    }
}

/// Materialize a built-in mode as an [`agent::subagent::Mode`] for the frontend.
pub fn builtin_modes() -> Vec<Mode> {
    ["ask", "debug", "edit"]
        .into_iter()
        .map(|name| Mode {
            name: name.to_string(),
            description: match name {
                "ask" => "Read-only Q&A: inspect the codebase and answer, no edits.".to_string(),
                "debug" => "Debug: reproduce, diagnose from evidence, then propose a fix.".to_string(),
                "edit" => "Code-Edit-Only: read, edit files, run checks — no terminal or git writes.".to_string(),
                _ => String::new(),
            },
            system_prompt: builtin_mode_system_prompt(name).unwrap_or_default().to_string(),
            allowed_tools: builtin_mode_tools(name).unwrap_or_default().into_iter().map(str::to_string).collect(),
            allowed_globs: None,
            model_override: None,
        })
        .collect()
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
                allowed_globs: m.allowed_globs.clone(),
                model_override: m.model_override.clone(),
            })
            .collect();
    });
}

/// The name of the custom mode currently running on this thread (depth ≥ 1),
/// if the profile was built from a `.ai/modes/*.md` file (custom modes), as
/// opposed to a built-in profile. Built-in children return `None`.
fn current_custom_mode() -> Option<String> {
    let profile = current_profile()?;
    let is_builtin = matches!(
        profile.as_str(),
        "explore" | "review" | "implement" | "debug"
    );
    if is_builtin {
        return None;
    }
    Some(profile)
}

/// The `model_override` declared by the active custom mode on this thread
/// (`.ai/modes/*.md` `model:` frontmatter), if any. Built-in profiles and
/// modes without a `model:` field return `None`. Custom modes carry their own
/// override so a mode can pin which model executes its work; the orchestrator
/// uses this when the parent hasn't supplied a per-`task` override.
pub fn current_mode_model_override() -> Option<String> {
    let mode_name = current_custom_mode()?;
    CUSTOM_MODES.with(|m| {
        let modes = m.borrow();
        modes
            .iter()
            .find(|mm| mm.name == mode_name)
            .and_then(|mm| mm.model_override.clone())
    })
}

// ---------------------------------------------------------------------------
// Workflow tool restrictions (`.ai/workflows/*.md` `allowedTools:`).
//
// Workflows reuse the custom-mode tool gate so `/workflow-name` runs are
// scoped to the workflow's declared tools. A workflow is registered under its
// name for a single invocation; entries left in the registry after the run are
// cleared by the next call. This keeps enforcement centralized in one place.
// ---------------------------------------------------------------------------

thread_local! {
    static WORKFLOW_TOOLS: RefCell<Vec<(String, Vec<String>)>> = const { RefCell::new(Vec::new()) };
}

/// Register a workflow's tool allow-list for the current execution scope.
/// Calling with an empty list clears any earlier entry for `name` (restricted
/// becomes unrestricted). Multiple registrations accumulate; enforcement looks
/// up the most recently registered entry by name.
pub fn register_workflow_tools(name: &str, allowed_tools: Vec<String>) {
    WORKFLOW_TOOLS.with(|w| {
        let mut guard = w.borrow_mut();
        guard.retain(|(n, _)| n != name);
        if !allowed_tools.is_empty() {
            guard.push((name.to_string(), allowed_tools));
        }
    });
}

/// Tool-call verdict for a workflow-restricted child: deny `tool` when the
/// current name's workflow declares a non-empty allow-list that excludes it.
/// Returns `None` when unrestricted (no workflow or empty allow-list).
pub fn workflow_child_tool_verdict(name: &str, tool: &str) -> Option<String> {
    let allowed = WORKFLOW_TOOLS.with(|w| {
        w.borrow()
            .iter()
            .rev()
            .find(|(n, _)| n == name)
            .map(|(_, tools)| tools.clone())
    });
    match allowed {
        None => None,
        Some(tools) if tools.is_empty() => None, // explicit unrestricted
        Some(tools) if tools.iter().any(|t| t == tool) => None,
        Some(tools) => Some(format!(
            "Tool `{tool}` is not available to the `{name}` workflow (allowedTools: {}). \
             Complete the step within the workflow's allowed tool set.",
            tools.join(", ")
        )),
    }
}

/// Deny a file-mutating `ToolCall` whose target `path` is not covered by the
/// active custom mode's `allowed_globs` (`.ai/modes/*.md` `globs:` field).
///
/// - Outside a subagent loop (depth 0) → always `None` (no restriction).
/// - Built-in profiles (explore/review/implement) → `None` (no globs defined).
/// - Custom mode with **no** `globs:` declared → `None` (globs are opt-in;
///   the `allowedTools:` allow-list still gates *which* tools run).
/// - Custom mode with globs + in-workspace `path` not matching any glob →
///   `Deny(reason)` so the child stays inside the mode's declared files.
///
/// Paths are matched against the workspace root so `globs:` like `"*.rs"`
/// mean "only `.rs` files inside this workspace". Only the `path`-bearing
/// mutating tools are gated (write/apply/create/delete); read tools are not
/// file-scoped by globs.
pub fn child_glob_verdict(workspaces: &[std::path::PathBuf], call: &ToolCall) -> Option<String> {
    let targets = glob_scoped_targets(call);
    if targets.is_empty() {
        return None;
    }
    let mode_name = current_custom_mode()?;
    let (globs, name) = CUSTOM_MODES.with(|m| {
        let modes = m.borrow();
        modes
            .iter()
            .find(|mm| mm.name == mode_name)
            .map(|mm| (mm.allowed_globs.clone(), mm.name.clone()))
    })?;
    let Some(patterns) = globs else {
        return None;
    };
    if patterns.is_empty() {
        return None;
    }
    // Resolve the workspace root to anchor relative globs.
    let root = workspaces.first()?;
    let mut first_denied: Option<String> = None;
    for path in targets {
        let Some(rel) = relative_to(root, &path) else {
            continue;
        };
        let mut matched = false;
        for pat in &patterns {
            if glob_match(pat, &rel) {
                matched = true;
                break;
            }
        }
        if !matched {
            first_denied = Some(path);
            break;
        }
    }
    let denied = first_denied?;
    Some(format!(
        "Path `{denied}` is outside the `{name}` mode's allowedGlobs (`{}`). \
         This mode may only touch files matching its declared globs.",
        patterns.join(", ")
    ))
}

/// The file-mutation target `path`(s) of a tool call, if it is a path-bearing
/// mutating tool. Read tools and shell are not file-scoped by globs.
fn glob_scoped_targets(call: &ToolCall) -> Vec<String> {
    match call {
        ToolCall::WriteFile { path, .. }
        | ToolCall::ApplyFileDiff { path, .. }
        | ToolCall::CreateFolder { path }
        | ToolCall::DeleteFileOrFolder { path } => vec![path.clone()],
        _ => Vec::new(),
    }
}

/// Compute `path` relative to `root`, if `path` is inside `root`.
fn relative_to(root: &std::path::Path, path: &str) -> Option<String> {
    let p = std::path::Path::new(path);
    if !p.is_absolute() {
        // Workspace-relative path — normalize and return as-is.
        let norm = p
            .components()
            .map(|c| c.as_os_str().to_string_lossy().to_string())
            .collect::<Vec<_>>()
            .join("/");
        return Some(norm);
    }
    let stripped = p.strip_prefix(root).ok()?;
    Some(
        stripped
            .components()
            .map(|c| c.as_os_str().to_string_lossy().to_string())
            .collect::<Vec<_>>()
            .join("/"),
    )
}

/// A tiny glob matcher supporting `*` (within a path segment) and `**` (across
/// segments). No external dependency; sufficient for `.ai/modes/*.md` `globs:`
/// like `"*.rs"`, `"src/**"`, `"**/*.test.ts"`.
fn glob_match(pat: &str, text: &str) -> bool {
    let pat = pat.trim();
    if pat.is_empty() {
        return false;
    }
    // Split into segments; `**` may span multiple segments.
    let psegs: Vec<&str> = pat.split('/').collect();
    let tsegs: Vec<&str> = text.split('/').collect();
    if psegs.iter().any(|s| *s == "**") {
        // `**` present — search a start offset so the double-star can swallow
        // any number of segments (greedy backtracking, bounded by text length).
        return glob_with_doublestar(&psegs, &tsegs, 0, 0, 0);
    }
    if psegs.len() != tsegs.len() {
        return false;
    }
    psegs.iter().zip(tsegs.iter()).all(|(p, t)| seg_match(p, t))
}

fn glob_with_doublestar(psegs: &[&str], tsegs: &[&str], pi: usize, ti: usize, depth: usize) -> bool {
    if depth > tsegs.len() {
        return false;
    }
    if pi == psegs.len() {
        return ti == tsegs.len();
    }
    if psegs[pi] == "**" {
        // Either consume one text segment (stay on `**`) or move past it.
        return glob_with_doublestar(psegs, tsegs, pi + 1, ti, depth + 1)
            || (ti < tsegs.len() && glob_with_doublestar(psegs, tsegs, pi, ti + 1, depth + 1));
    }
    if ti >= tsegs.len() {
        return false;
    }
    seg_match(psegs[pi], tsegs[ti]) && glob_with_doublestar(psegs, tsegs, pi + 1, ti + 1, depth)
}

/// Match a single path segment against a pattern (`*` = any run of non-`/`).
fn seg_match(pat: &str, text: &str) -> bool {
    if !pat.contains('*') {
        return pat == text;
    }
    // Leading literal must match from the start.
    let parts: Vec<&str> = pat.split('*').collect();
    let (first, last) = (parts.first().unwrap_or(&""), parts.last().unwrap_or(&""));
    if !first.is_empty() && !text.starts_with(first) {
        return false;
    }
    if !last.is_empty() && !text.ends_with(last) {
        return false;
    }
    // Middle `*` gaps just need to exist; with prefix+suffix already satisfied,
    // a single `*` always matches the remainder, and multiple middles only matter
    // for ordered infix checks. Here we only need the wildcard semantics.
    if parts.len() == 2 {
        return true;
    }
    // Multiple stars: verify the ordered infix substrings appear left-to-right
    // strictly within the region between prefix and suffix.
    let inner = &text[first.len()..text.len().saturating_sub(last.len())];
    let mut rest = inner;
    for mid in parts[1..parts.len() - 1].iter().filter(|p| !p.is_empty()) {
        match rest.find(mid) {
            Some(idx) => rest = &rest[idx + mid.len()..],
            None => return false,
        }
    }
    true
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
        // `debug` is read-only for files but may run tests/commands for evidence.
        assert!(tool_allowed("debug", "read_file_range"));
        assert!(tool_allowed("debug", "run_tests"));
        assert!(tool_allowed("debug", "execute_terminal_command"));
        assert!(!tool_allowed("debug", "write_file"), "debug may not edit");
    }

    #[test]
    fn debug_profile_is_delegatable() {
        let profile = lookup("debug").expect("debug profile resolves");
        assert!(profile.max_steps > 0);
        assert!(profile.system_prompt.contains("hypothesis"));
        let catalog = tool_catalog_markdown("debug");
        assert!(catalog.contains("execute_terminal_command"));
        assert!(catalog.contains("run_tests"));
        assert!(!catalog.contains("write_file"));
        assert!(!catalog.contains("task"));
    }

    #[test]
    fn builtin_ask_mode_is_read_only() {
        assert!(tool_allowed("ask", "read_file_range"));
        assert!(tool_allowed("ask", "glob_search_codebase"));
        assert!(tool_allowed("ask", "git_diff"));
        assert!(!tool_allowed("ask", "write_file"), "ask must be read-only");
        assert!(
            !tool_allowed("ask", "apply_file_diff"),
            "ask must not edit files"
        );
        assert!(
            !tool_allowed("ask", "execute_terminal_command"),
            "ask must not mutate"
        );
    }

    #[test]
    fn builtin_modes_are_served_and_debug_adds_diagnostics() {
        let builtin = builtin_modes();
        let names: Vec<&str> = builtin.iter().map(|m| m.name.as_str()).collect();
        assert!(names.contains(&"ask"));
        assert!(names.contains(&"debug"));
        assert!(tool_allowed("debug", "run_tests"));
        assert!(tool_allowed("debug", "read_file_range"));
        assert!(!tool_allowed("debug", "write_file"), "debug is not an editor");
    }

    #[test]
    fn no_child_may_delegate_or_commit_or_delete() {
        for profile in ["explore", "implement", "review", "debug"] {
            for tool in child_never() {
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
    fn custom_mode_allowed_globs_deny_out_of_scope_writes() {
        let root = std::env::temp_dir().join(format!("ai-globs-ws-{}", std::process::id()));
        std::fs::create_dir_all(root.join("src")).unwrap();
        let modes_dir = root.join(".ai").join("modes");
        std::fs::create_dir_all(&modes_dir).unwrap();
        std::fs::write(
            modes_dir.join("rusty.md"),
            "---\nname: rusty\ndescription: Rust-only editor\nallowedTools: write_file, apply_file_diff\n\
             globs: *.rs, src/**\n---\nOnly touch Rust files.",
        )
        .unwrap();
        let modes = load_modes(&root);
        let rusty = modes.iter().find(|m| m.name == "rusty").unwrap();
        assert_eq!(
            rusty.allowed_globs.as_deref(),
            Some(&vec!["*.rs".to_string(), "src/**".to_string()][..])
        );
        register_modes(&modes);

        // No child context → no restriction.
        let ws = vec![root.clone()];
        let out_rs = ToolCall::WriteFile {
            path: root.join("out.rs").to_string_lossy().to_string(),
            content: "fn main() {}".into(),
        };
        assert!(child_glob_verdict(&ws, &out_rs).is_none());

        // Inside the `rusty` custom-mode child, `*.rs` at the workspace root is
        // allowed…
        let _guard = enter_child(build_mode_profile(rusty)).unwrap();
        assert!(child_glob_verdict(&ws, &out_rs).is_none());
        // …a nested `src/*` file matches `src/**`…
        let nested = ToolCall::WriteFile {
            path: root.join("src/lib.rs").to_string_lossy().to_string(),
            content: "pub fn f() {}".into(),
        };
        assert!(child_glob_verdict(&ws, &nested).is_none());
        // …but a `.py` file the mode never declared is denied.
        let py = ToolCall::WriteFile {
            path: root.join("out.py").to_string_lossy().to_string(),
            content: "print('hi')".into(),
        };
        let reason = child_glob_verdict(&ws, &py);
        assert!(
            matches!(reason.as_deref(), Some(r) if r.contains("out.py") && r.contains("allowedGlobs"))
        );

        // Relative path form (`src/lib.rs`) is also scope-checked.
        let rel = ToolCall::WriteFile {
            path: "src/lib.rs".into(),
            content: "pub fn g() {}".into(),
        };
        assert!(child_glob_verdict(&ws, &rel).is_none());

        // A custom mode WITHOUT globs is unrestricted by globs (tool list only).
        std::fs::write(
            modes_dir.join("any.md"),
            "---\nname: any\ndescription: Anything\nallowedTools: write_file\n---\nGo.",
        )
        .unwrap();
        let modes2 = load_modes(&root);
        let any = modes2.iter().find(|m| m.name == "any").unwrap();
        register_modes(&modes2);
        let _guard2 = enter_child(build_mode_profile(any)).unwrap();
        let txt = ToolCall::WriteFile {
            path: root.join("out.txt").to_string_lossy().to_string(),
            content: "x".into(),
        };
        assert!(child_glob_verdict(&ws, &txt).is_none());

        // Clear for other tests.
        register_modes(&[]);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn custom_mode_model_override_exposed_for_child() {
        let root = std::env::temp_dir().join(format!("ai-mo-ws-{}", std::process::id()));
        let modes_dir = root.join(".ai").join("modes");
        std::fs::create_dir_all(&modes_dir).unwrap();
        std::fs::write(
            modes_dir.join("fast.md"),
            "---\nname: fast\ndescription: Fast model\nmodel: fast-gguf.gguf\n---\nBe quick.",
        )
        .unwrap();
        let modes = load_modes(&root);
        assert_eq!(
            modes.iter().find(|m| m.name == "fast").unwrap().model_override.as_deref(),
            Some("fast-gguf.gguf")
        );
        register_modes(&modes);

        // Outside a custom-mode child → None.
        assert!(current_mode_model_override().is_none());

        // In parent context (depth 0, no profile) → None.
        let _g = enter_child(build_mode_profile(modes.iter().find(|m| m.name == "fast").unwrap()))
            .unwrap();
        assert_eq!(
            current_mode_model_override().as_deref(),
            Some("fast-gguf.gguf")
        );
        drop(_g);

        // Built-in profiles never expose a mode override.
        let _g = enter_child(lookup("implement").unwrap()).unwrap();
        assert_eq!(current_mode_model_override(), None);

        register_modes(&[]);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn glob_matcher_smoke() {
        // `*` within a segment.
        assert!(glob_match("*.rs", "lib.rs"));
        assert!(!glob_match("*.rs", "lib.py"));
        assert!(glob_match("test.*", "test.ts"));
        // `**` across segments.
        assert!(glob_match("src/**", "src/a/b/lib.rs"));
        assert!(glob_match("src/**", "src/lib.rs"));
        assert!(!glob_match("src/**", "tests/lib.rs"));
        assert!(glob_match("**/*.test.ts", "src/components/Foo.test.ts"));
        assert!(glob_match("**/*.test.ts", "Foo.test.ts"));
        // Mixed.
        assert!(glob_match("src/**/*.rs", "src/generated/parser.rs"));
        assert!(!glob_match("src/**/*.rs", "src/generated/parser.py"));
    }

    #[test]
    fn missing_modes_dir_returns_empty() {
        let dir = std::env::temp_dir().join(format!("ai-modes-empty-{}", std::process::id()));
        let modes = load_modes(&dir);
        assert!(modes.is_empty());
    }
}
