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
pub fn tool_schemas() -> HashMap<&'static str, Value> {
    let mut m = HashMap::new();
    m.insert(
        "glob_search_codebase",
        json_schema(vec![
            prop("pattern", "string", "Glob pattern relative to the workspace root, e.g. `**/*.rs` or `src/**/main.*`."),
            prop_opt("root", "string", "Absolute directory to search (defaults to the workspace root)."),
            prop_opt("respect_gitignore", "boolean", "Whether to skip paths ignored by git (default true)."),
        ]),
    );
    m.insert(
        "search_file_contents",
        json_schema(vec![
            prop("pattern", "string", "Regular expression to match against file contents, e.g. `fn connect_db` or `TODO.*fixme`."),
            prop_opt("include", "string", "Optional glob to restrict which files are searched (e.g. `src/**/*.rs`). Defaults to all files in the workspace."),
            prop_opt("root", "string", "Absolute directory to search (defaults to the workspace root)."),
            prop_opt("respect_gitignore", "boolean", "Whether to skip paths ignored by git (default true)."),
        ]),
    );
    m.insert(
        "view_file_structure",
        json_schema(vec![
            prop("path", "string", "Absolute path to the source file to parse."),
            prop_opt("max_depth", "integer", "Maximum AST depth to scan for declarations (default 4)."),
        ]),
    );
    m.insert(
        "read_file_range",
        json_schema(vec![
            prop("path", "string", "Absolute path to the file."),
            prop("start_line", "integer", "First 1-based line to read."),
            prop("end_line", "integer", "Last 1-based line to read (clamped to EOF)."),
        ]),
    );
    m.insert(
        "apply_file_diff",
        json_schema(vec![
            prop("path", "string", "Absolute path to the file to edit."),
            prop("diff", "string", "A SEARCH/REPLACE block:\n\n<diff>\n@@\nSEARCH:\n<existing lines>\nREPLACE:\n<new lines>\n</diff>\n\nBoth SEARCH and REPLACE are required and must be separated by exactly one blank line."),
        ]),
    );
    m.insert(
        "execute_terminal_command",
        json_schema(vec![
            prop("command", "string", "Shell command to run. Avoid destructive or interactive commands."),
            prop_opt("timeout_secs", "integer", "Timeout in seconds (1..300, default 30)."),
            prop_opt("cwd", "string", "Working directory for the command (defaults to workspace root)."),
        ]),
    );
    m.insert(
        "call_mcp_tool",
        json_schema(vec![
            prop("server_bin", "string", "Executable path of the MCP server (stdio transport)."),
            prop_opt("server_args", "array", "Command-line arguments for the MCP server."),
            prop("tool", "string", "Tool name exposed by the MCP server."),
            prop_opt("arguments", "object", "JSON arguments for the tool call."),
            prop_opt("timeout_secs", "integer", "Timeout in seconds (default 30)."),
        ]),
    );
    m.insert(
        "git_status",
        json_schema(vec![prop_opt("git_status", "boolean", "Whether to return the porcelain status (default true).")]),
    );
    m.insert(
        "git_diff",
        json_schema(vec![
            prop_opt("path", "string", "Optional path to scope the diff to a single file (relative to workspace root)."),
        ]),
    );
    m.insert(
        "git_commit",
        json_schema(vec![
            prop("message", "string", "Commit message describing the change."),
        ]),
    );
    m.insert(
        "git_checkpoint",
        json_schema(vec![
            prop_opt("message", "string", "Optional checkpoint label; auto-generates a message when omitted."),
        ]),
    );
    m.insert(
        "git_revert",
        json_schema(vec![
            prop_opt("commit", "string", "The checkpoint commit to revert to (defaults to the most recent checkpoint)."),
        ]),
    );
    m.insert(
        "run_tests",
        json_schema(vec![
            prop_opt("command", "string", "Explicit test command. Omit to auto-detect (`npm test`, `cargo test`, …) from the workspace."),
        ]),
    );
    m.insert(
        "write_file",
        json_schema(vec![
            prop("path", "string", "Absolute path of the file to write (must be inside the workspace)."),
            prop("content", "string", "Full new file content."),
        ]),
    );
    m.insert(
        "create_skill",
        json_schema(vec![
            prop("name", "string", "Short, descriptive skill name (e.g. `build-react-table`, `reproduce-bug`)."),
            prop_opt("description", "string", "One-line description of when to use this skill."),
            prop("content", "string", "Markdown body of the skill: the reusable procedure, checklist, or playbook you learned."),
        ]),
    );
    m.insert(
        "read_skill",
        json_schema(vec![
            prop("name", "string", "Name of an available skill (from the ## Skill instructions context section) to load in full. Use this when a skill was truncated to save context or you need its complete text before applying it."),
        ]),
    );
    m.insert(
        "semantic_search_codebase",
        json_schema(vec![
            prop("query", "string", "Natural-language query describing what you're looking for, e.g. `where is the auth logic` or `how are timestamps formatted`. Results are ranked by code-aware relevance, not literal match."),
            prop_opt("include", "string", "Optional glob to restrict which files are indexed (e.g. `src/**/*.rs`). Defaults to all files in the workspace."),
            prop_opt("root", "string", "Absolute directory to search (defaults to the workspace root)."),
            prop_opt("respect_gitignore", "boolean", "Whether to skip paths ignored by git (default true)."),
            prop_opt("top_k", "integer", "How many ranked regions to return (default 10, max 25)."),
        ]),
    );
    m.insert(
        "create_plan",
        json_schema(vec![
            prop("title", "string", "Short title for the plan, e.g. `Refactor auth`."),
            prop_opt("goal", "string", "One sentence describing the overall goal the plan accomplishes."),
            prop("items", "array", "The ordered list of concrete steps to take. Each item should be a single self-contained directive the agent can execute."),
        ]),
    );
    m.insert(
        "read_plan",
        json_schema(vec![]),
    );
    m.insert(
        "update_plan",
        json_schema(vec![
            prop("item", "integer", "1-based index of the plan item to update."),
            prop("status", "string", "New status: `not_started`, `in_progress`, `completed` or `terminal`."),
            prop_opt("details", "string", "Optional extra detail/result to record for the item (appended)."),
        ]),
    );
    m.insert(
        "execute_plan",
        json_schema(vec![
            prop_opt("execute_plan", "boolean", "Whether to begin executing the active plan (default true)."),
        ]),
    );
    m
}

fn json_schema(props: Vec<Value>) -> Value {
    let mut required = Vec::new();
    let mut object = serde_json::Map::new();
    for p in props {
        let mut p_obj = p.as_object().unwrap().clone();
        let name = p_obj.get("name").unwrap().as_str().unwrap().to_string();
        let is_optional = p_obj
            .get("optional")
            .map(|o| o.as_bool().unwrap_or(false))
            .unwrap_or(false);
        p_obj.remove("optional");
        p_obj.remove("name");
        if !is_optional {
            required.push(name.clone());
        }
        object.insert(name, Value::Object(p_obj));
    }
    serde_json::json!({
        "type": "object",
        "properties": object,
        "required": required,
        "additionalProperties": false
    })
}

fn prop(name: &str, ty: &str, desc: &str) -> Value {
    serde_json::json!({ "name": name, "type": ty, "description": desc })
}

fn prop_opt(name: &str, ty: &str, desc: &str) -> Value {
    serde_json::json!({ "name": name, "type": ty, "description": desc, "optional": true })
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
}
