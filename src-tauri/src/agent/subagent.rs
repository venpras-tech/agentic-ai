//! P1-8 first-class subagents.
//!
//! A subagent is a named profile (`explore`, `implement`, `review`) that runs
//! its own focused tool loop on a spare engine worker and reports distilled
//! findings back to the parent agent as one tool observation. Profiles differ
//! in mission, step budget, and tool guidance — the parent's context stays
//! clean instead of absorbing raw file dumps.
//!
//! v1 scope: policy enforcement stays centralized in `policy::check` (child
//! calls go through the same gate as parent calls); per-profile *tool
//! restrictions* are guidance-level, enforced via the system prompt.

/// A reusable specialist the parent agent can delegate focused work to.
pub struct SubagentProfile {
    pub name: &'static str,
    /// One-line capability summary surfaced to the parent model.
    pub description: &'static str,
    /// System instructions for the child loop.
    pub system_prompt: &'static str,
    /// Hard cap on tool-call rounds for the child (keeps runaway children
    /// from eating the whole budget).
    pub max_steps: usize,
}

pub const PROFILES: &[SubagentProfile] = &[
    SubagentProfile {
        name: "explore",
        description: "Read-only codebase reconnaissance: find where things live, map call paths, summarize architecture.",
        system_prompt: "You are an EXPLORATION subagent. Mission: investigate the codebase and answer the parent's task with precise, citable findings.\n\
            Use ONLY read tools (glob_search_codebase, search_file_contents, view_file_structure, read_file_range, read_file_chars, list_dir).\n\
            Do NOT modify files or run shell commands. Cite exact `path:line` references. Finish with a compact findings report; omit raw file dumps.",
        max_steps: 4,
    },
    SubagentProfile {
        name: "implement",
        description: "Focused implementation of one well-scoped change: edit code, then verify it compiles/tests.",
        system_prompt: "You are an IMPLEMENTATION subagent. Mission: complete exactly ONE well-scoped change end-to-end.\n\
            Read before writing; keep edits minimal and idiomatic; run tests/typecheck after editing when possible.\n\
            Do NOT start unrelated work. Finish with a short report: what changed (paths), how it was verified, any follow-ups.",
        max_steps: 6,
    },
    SubagentProfile {
        name: "review",
        description: "Adversarial review of recent changes or specified files: bugs, edge cases, style violations.",
        system_prompt: "You are a CODE REVIEW subagent. Mission: scrutinize the specified changes/files for correctness bugs, missing edge cases, security issues, and convention violations.\n\
            Use ONLY read tools plus git_diff/git_status for context. Do NOT modify anything.\n\
            Report findings ordered by severity with `path:line` references; say explicitly when something is fine.",
        max_steps: 4,
    },
];

/// Resolve a profile by name (case-insensitive).
pub fn lookup(name: &str) -> Option<&'static SubagentProfile> {
    let lower = name.trim().to_ascii_lowercase();
    PROFILES.iter().find(|p| p.name == lower)
}

/// Comma-separated profile list for the parent system prompt.
pub fn catalog() -> String {
    PROFILES
        .iter()
        .map(|p| format!("- `{}`: {}", p.name, p.description))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_is_case_insensitive_and_total_over_catalog() {
        assert_eq!(lookup("EXPLORE").unwrap().name, "explore");
        assert!(lookup("  implement ").is_some());
        assert!(lookup("review").is_some());
        assert!(lookup("nonexistent").is_none());
        // Every catalog entry resolves (guards typo'd additions).
        for p in PROFILES {
            assert!(lookup(p.name).is_some());
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
}
