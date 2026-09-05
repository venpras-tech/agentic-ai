//! Markdown `<execute_tool>` tag parsing + tool JSON schemas.
//!
//! The orchestrator model emits tool calls as fenced tags inside Markdown:
//!
//! ```markdown
//! I'll inspect the workspace first.
//!
//! <execute_tool>
//! { "type": "glob_search_codebase", "pattern": "**/*.ts", "root": null, "respect_gitignore": true }
//! </execute_tool>
//!
//! Found 3 TypeScript entry points…
//! ```
//!
//! [`parse_tool_calls`] extracts every `execute_tool` block from a Markdown
//! stream and deserializes each payload into a [`ToolCall`].

use std::collections::HashMap;

use serde_json::Value;

use super::ToolCall;

/// Extract every `<execute_tool>…</execute_tool>` block from `markdown`.
///
/// Blocks may contain a JSON object directly, or a fenced JSON code block
/// inside. Invalid blocks are skipped (with a warning emitted through
/// `on_warn`), so one malformed tag never aborts the whole batch.
pub fn parse_tool_calls(markdown: &str, on_warn: &mut dyn FnMut(String)) -> Vec<ToolCall> {
    let mut calls = Vec::new();
    let mut rest = markdown;
    while let Some(start) = rest.find("<execute_tool>") {
        let after = &rest[start + "<execute_tool>".len()..];
        let Some(end_rel) = after.find("</execute_tool>") else {
            on_warn("Unclosed <execute_tool> tag ignored".to_string());
            break;
        };
        let payload = &after[..end_rel];
        let trimmed = payload.trim();
        let body = strip_json_fence(trimmed);
        match serde_json::from_str::<ToolCall>(body) {
            Ok(call) => calls.push(call),
            Err(e) => on_warn(format!("Skipping invalid <execute_tool>: {e}")),
        }
        rest = &after[end_rel + "</execute_tool>".len()..];
    }
    calls
}

/// Allow the payload to be wrapped in a ```json fenced block for tokenizer
/// friendliness.
fn strip_json_fence(body: &str) -> &str {
    let mut b = body.trim();
    if let Some(stripped) = b.strip_prefix("```json") {
        b = stripped.trim_start();
    } else if let Some(stripped) = b.strip_prefix("```") {
        b = stripped.trim_start();
    }
    if let Some(stripped) = b.strip_suffix("```") {
        b = stripped.trim_end();
    }
    b.trim()
}

/// Count how many characters of `needle` appear verbatim in `haystack` - used
/// by `count_occurrences` in apply_file_diff to report ambiguous matches.
#[allow(dead_code)]
pub fn count_occurrences(haystack: &str, needle: &str) -> usize {
    if needle.is_empty() {
        return 0;
    }
    let mut count = 0;
    let mut rest = haystack;
    while let Some(pos) = rest.find(needle) {
        count += 1;
        rest = &rest[pos + needle.len()..];
    }
    count
}

/// JSON schema descriptions for each tool, for orchestrator-side validation.
///
/// Derived entirely from the canonical [`registry`](super::registry) table so
/// the schema set, the permission classes and the dispatcher can never drift:
/// every registered tool MUST carry a schema (enforced by
/// [`registry::validate`](super::registry::validate) via the drift-guard test),
/// which is exactly the property that used to be missing for
/// `analyze_bug` / `review_code` / `browse_web`.
pub fn tool_schemas() -> HashMap<&'static str, Value> {
    crate::agent::registry::tool_schemas()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_single_tag() {
        let md = "Let me look.\n\n<execute_tool>\n{ \"type\": \"glob_search_codebase\", \"pattern\": \"**/*.ts\", \"root\": null, \"respect_gitignore\": true }\n</execute_tool>\n\nDone.";
        let mut warns = Vec::new();
        let calls = parse_tool_calls(md, &mut |w| warns.push(w));
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name(), "glob_search_codebase");
        assert!(warns.is_empty());
    }

    #[test]
    fn parses_fenced_json() {
        let md = "<execute_tool>\n```json\n{\"type\":\"read_file_range\",\"path\":\"src/main.rs\",\"startLine\":1,\"endLine\":20}\n```\n</execute_tool>";
        let warns: Vec<String> = Vec::new();
        let calls = parse_tool_calls(md, &mut |_| {});
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name(), "read_file_range");
        assert!(warns.is_empty());
    }

    #[test]
    fn skips_invalid_and_continues() {
        let md = "<execute_tool>{not json}</execute_tool>\n<execute_tool>{\"type\":\"execute_terminal_command\",\"command\":\"echo hi\"}</execute_tool>";
        let mut warns: Vec<String> = Vec::new();
        let calls = parse_tool_calls(md, &mut |w| warns.push(w));
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name(), "execute_terminal_command");
        assert_eq!(warns.len(), 1);
    }

    #[test]
    fn parses_task_call_with_and_without_profile() {
        let md = "<execute_tool>{\"type\":\"task\",\"subagentType\":\"review\",\"task\":\"Review src/main.rs for bugs\"}</execute_tool>";
        let calls = parse_tool_calls(md, &mut |_| {});
        assert_eq!(calls.len(), 1);
        match &calls[0] {
            ToolCall::Task {
                subagent_type,
                task,
                ..
            } => {
                assert_eq!(subagent_type.as_deref(), Some("review"));
                assert_eq!(task, "Review src/main.rs for bugs");
            }
            other => panic!("expected Task, got {}", other.name()),
        }

        // Profile omitted → None (defaults to explore at dispatch).
        let calls = parse_tool_calls(
            "<execute_tool>{\"type\":\"task\",\"task\":\"Map the repo\"}</execute_tool>",
            &mut |_| {},
        );
        match &calls[0] {
            ToolCall::Task { subagent_type, .. } => assert_eq!(*subagent_type, None),
            other => panic!("expected Task, got {}", other.name()),
        }
    }

    #[test]
    fn task_schema_lists_only_task_props() {
        let schema = tool_schemas().get("task").cloned().unwrap();
        let required = schema["required"].as_array().unwrap();
        assert_eq!(required.len(), 1);
        assert_eq!(required[0], "task");
        assert!(schema["properties"]["subagent_type"].is_object());
    }

    #[test]
    fn parses_ask_question_with_and_without_choices() {
        let calls = parse_tool_calls(
            "<execute_tool>{\"type\":\"ask_question\",\"question\":\"Postgres or SQLite?\",\"choices\":[\"Postgres\",\"SQLite\"]}</execute_tool>",
            &mut |_| {},
        );
        match &calls[0] {
            ToolCall::AskQuestion { question, choices } => {
                assert_eq!(question, "Postgres or SQLite?");
                assert_eq!(
                    choices.as_deref(),
                    Some(&["Postgres".to_string(), "SQLite".to_string()][..])
                );
            }
            other => panic!("expected AskQuestion, got {}", other.name()),
        }
        let calls = parse_tool_calls(
            "<execute_tool>{\"type\":\"ask_question\",\"question\":\"Proceed?\"}</execute_tool>",
            &mut |_| {},
        );
        match &calls[0] {
            ToolCall::AskQuestion { choices, .. } => assert_eq!(*choices, None),
            other => panic!("expected AskQuestion, got {}", other.name()),
        }
    }

    #[test]
    fn parses_new_git_and_lint_calls() {
        let cases = [
            (
                "{\"type\":\"git_blame\",\"path\":\"src/lib.ts\",\"startLine\":10,\"endLine\":40}",
                "git_blame",
            ),
            ("{\"type\":\"git_push\",\"branch\":\"feat/x\"}", "git_push"),
            ("{\"type\":\"git_pull\"}", "git_pull"),
            (
                "{\"type\":\"git_create_branch\",\"name\":\"feat/y\"}",
                "git_create_branch",
            ),
            ("{\"type\":\"git_pr_status\"}", "git_pr_status"),
            ("{\"type\":\"git_ci_status\"}", "git_ci_status"),
            (
                "{\"type\":\"create_pr\",\"title\":\"T\",\"body\":\"B\"}",
                "create_pr",
            ),
            (
                "{\"type\":\"read_lints\",\"path\":\"a/b.rs\"}",
                "read_lints",
            ),
            (
                "{\"type\":\"send_to_user\",\"message\":\"hi\"}",
                "send_to_user",
            ),
        ];
        for (payload, expected) in cases {
            let md = format!("<execute_tool>{payload}</execute_tool>");
            let calls = parse_tool_calls(&md, &mut |_| {});
            assert_eq!(calls.len(), 1, "case {expected}");
            assert_eq!(calls[0].name(), expected);
        }
    }

    #[test]
    fn new_tool_schemas_registered() {
        let schemas = tool_schemas();
        for name in [
            "git_blame",
            "git_push",
            "git_pull",
            "git_create_branch",
            "git_pr_status",
            "git_ci_status",
            "create_pr",
            "read_lints",
            "ask_question",
            "send_to_user",
        ] {
            assert!(schemas.contains_key(name), "missing schema for {name}");
        }
        // ask_question requires only `question`; git_blame only `path`.
        let aq = schemas.get("ask_question").cloned().unwrap();
        assert_eq!(aq["required"].as_array().unwrap().len(), 1);
        assert_eq!(aq["required"][0], "question");
        assert!(aq["properties"]["choices"].is_object());
        let blame = schemas.get("git_blame").cloned().unwrap();
        assert_eq!(blame["required"][0], "path");
    }

    #[test]
    fn test_registry_matches_dispatcher() {
        use crate::agent::registry;
        use crate::agent::ToolCall;

        // The registry (metadata + schemas) must name exactly the tools the
        // dispatcher can execute — no more, no less.
        let names = ToolCall::all_tool_names();
        registry::validate(&names).expect("registry must match the dispatcher exactly");

        // Every dispatcher tool must be advertised to the model (have a
        // schema) — the drift that previously hidden `analyze_bug`,
        // `review_code` and `browse_web` from agent mode.
        let schemas = tool_schemas();
        for name in &names {
            assert!(
                schemas.contains_key(*name),
                "dispatcher tool `{name}` has no schema — the model cannot call it"
            );
        }
        for name in schemas.keys() {
            assert!(
                names.iter().any(|n| n == name),
                "schema advertised for `{name}` but the dispatcher has no arm for it"
            );
        }
    }
}
