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
            prop(
                "pattern",
                "string",
                "Glob pattern relative to the workspace root, e.g. `**/*.rs` or `src/**/main.*`.",
            ),
            prop_opt(
                "root",
                "string",
                "Absolute directory to search (defaults to the workspace root).",
            ),
            prop_opt(
                "respect_gitignore",
                "boolean",
                "Whether to skip paths ignored by git (default true).",
            ),
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
            prop(
                "path",
                "string",
                "Absolute path to the source file to parse.",
            ),
            prop_opt(
                "max_depth",
                "integer",
                "Maximum AST depth to scan for declarations (default 4).",
            ),
        ]),
    );
    m.insert(
        "read_file_range",
        json_schema(vec![
            prop("path", "string", "Absolute path to the file."),
            prop("start_line", "integer", "First 1-based line to read."),
            prop(
                "end_line",
                "integer",
                "Last 1-based line to read (clamped to EOF).",
            ),
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
            prop(
                "command",
                "string",
                "Shell command to run. Avoid destructive or interactive commands.",
            ),
            prop_opt(
                "timeout_secs",
                "integer",
                "Timeout in seconds (1..300, default 30).",
            ),
            prop_opt(
                "cwd",
                "string",
                "Working directory for the command (defaults to workspace root).",
            ),
        ]),
    );
    m.insert(
        "call_mcp_tool",
        json_schema(vec![
            prop_opt("server", "string", "Name of a configured MCP server from the catalog (see list_mcp_servers)."),
            prop_opt("server_bin", "string", "Ad-hoc executable path of an MCP server (stdio transport); use when no catalog entry exists."),
            prop_opt("server_args", "array", "Command-line arguments for the ad-hoc MCP server executable."),
            prop("tool", "string", "Tool name exposed by the MCP server."),
            prop_opt("arguments", "object", "JSON arguments for the tool call."),
            prop_opt("timeout_secs", "integer", "Timeout in seconds (default 30)."),
        ]),
    );
    m.insert("list_mcp_servers", json_schema(vec![]));
    m.insert(
        "calculate",
        json_schema(vec![prop(
            "expression",
            "string",
            "Arithmetic expression to evaluate: numbers, + - * / % ^, parentheses. Example: \"(2+3)*4\".",
        )]),
    );
    m.insert(
        "add_mcp_server",
        json_schema(vec![
            prop(
                "name",
                "string",
                "Short unique catalog name, e.g. \"playwright\".",
            ),
            prop(
                "bin",
                "string",
                "Executable path or PATH command for the stdio MCP server.",
            ),
            prop_opt(
                "args",
                "array",
                "Command-line arguments for the executable.",
            ),
        ]),
    );
    m.insert(
        "remove_mcp_server",
        json_schema(vec![prop(
            "name",
            "string",
            "Catalog name of the server to remove.",
        )]),
    );
    m.insert(
        "attach_file",
        json_schema(vec![prop(
            "path",
            "string",
            "Absolute path of the text file to chunk + index for semantic search.",
        )]),
    );
    m.insert(
        "search_attached_files",
        json_schema(vec![
            prop(
                "query",
                "string",
                "Natural-language query over the attached files.",
            ),
            prop_opt(
                "top_k",
                "integer",
                "How many chunks to return (1..20, default 5).",
            ),
        ]),
    );
    m.insert(
        "detach_file",
        json_schema(vec![prop(
            "path",
            "string",
            "Path of a previously attached file to remove from the index.",
        )]),
    );
    m.insert(
        "transcribe_audio",
        json_schema(vec![
            prop(
                "path",
                "string",
                "Absolute path of an audio/video file (wav, mp3, m4a, webm…).",
            ),
            prop_opt(
                "language",
                "string",
                "ISO language hint, e.g. \"en\" or \"de\" (default: auto-detect).",
            ),
        ]),
    );
    m.insert(
        "git_status",
        json_schema(vec![prop_opt(
            "git_status",
            "boolean",
            "Whether to return the porcelain status (default true).",
        )]),
    );
    m.insert(
        "git_diff",
        json_schema(vec![prop_opt(
            "path",
            "string",
            "Optional path to scope the diff to a single file (relative to workspace root).",
        )]),
    );
    m.insert(
        "git_commit",
        json_schema(vec![prop(
            "message",
            "string",
            "Commit message describing the change.",
        )]),
    );
    m.insert(
        "git_checkpoint",
        json_schema(vec![prop_opt(
            "message",
            "string",
            "Optional checkpoint label; auto-generates a message when omitted.",
        )]),
    );
    m.insert(
        "git_revert",
        json_schema(vec![prop_opt(
            "commit",
            "string",
            "The checkpoint commit to revert to (defaults to the most recent checkpoint).",
        )]),
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
            prop(
                "path",
                "string",
                "Absolute path of the file to write (must be inside the workspace).",
            ),
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
    m.insert("read_plan", json_schema(vec![]));
    m.insert(
        "update_plan",
        json_schema(vec![
            prop(
                "item",
                "integer",
                "1-based index of the plan item to update.",
            ),
            prop(
                "status",
                "string",
                "New status: `not_started`, `in_progress`, `completed` or `terminal`.",
            ),
            prop_opt(
                "details",
                "string",
                "Optional extra detail/result to record for the item (appended).",
            ),
        ]),
    );
    m.insert(
        "execute_plan",
        json_schema(vec![prop_opt(
            "execute_plan",
            "boolean",
            "Whether to begin executing the active plan (default true).",
        )]),
    );
    m.insert(
        "list_dir",
        json_schema(vec![
            prop_opt("path", "string", "Directory to list (absolute, or relative to the workspace root). Defaults to the workspace root."),
        ]),
    );
    m.insert(
        "read_file_chars",
        json_schema(vec![
            prop("path", "string", "Absolute path to the text file."),
            prop_opt("offset", "integer", "0-based UTF-8 character offset to start reading from (default 0)."),
            prop_opt("limit", "integer", "Maximum characters to return (default 4000, max 24000). The result ends with <EOF> or a continuation hint."),
        ]),
    );
    m.insert(
        "create_folder",
        json_schema(vec![
            prop("path", "string", "Folder to create (mkdir -p semantics; absolute or relative to the workspace root). Depth is capped at 50 segments."),
        ]),
    );
    m.insert(
        "copy_file_or_folder",
        json_schema(vec![
            prop(
                "src",
                "string",
                "Source file or folder (copied recursively).",
            ),
            prop(
                "dst",
                "string",
                "Destination path; must not exist unless canOverwrite is true.",
            ),
            prop_opt(
                "can_overwrite",
                "boolean",
                "Replace an existing destination (default false).",
            ),
        ]),
    );
    m.insert(
        "move_file_or_folder",
        json_schema(vec![
            prop("src", "string", "Source file or folder to move/rename."),
            prop(
                "dst",
                "string",
                "Destination path; must not exist unless canOverwrite is true.",
            ),
            prop_opt(
                "can_overwrite",
                "boolean",
                "Replace an existing destination (default false).",
            ),
        ]),
    );
    m.insert(
        "delete_file_or_folder",
        json_schema(vec![
            prop("path", "string", "File or folder to delete. Folders are removed recursively; the item goes to the OS Trash so it stays recoverable."),
        ]),
    );
    m.insert("get_scratchpad_folder", json_schema(vec![]));
    m.insert(
        "set_todo_list",
        json_schema(vec![
            prop("items", "array", "The full todo list as short imperative strings, in execution order. Replaces any previous list."),
        ]),
    );
    m.insert("get_todo_list", json_schema(vec![]));
    m.insert(
        "mark_todo_item_done",
        json_schema(vec![prop(
            "item",
            "integer",
            "1-based index of the todo item to mark done.",
        )]),
    );
    m.insert(
        "web_search",
        json_schema(vec![
            prop("query", "string", "The web search query."),
            prop_opt(
                "max_results",
                "integer",
                "How many results to return (default 8, max 10).",
            ),
        ]),
    );
    m.insert(
        "web_extract",
        json_schema(vec![
            prop("url", "string", "Public http(s) URL of the page to fetch. Private/loopback hosts and embedded credentials are rejected."),
        ]),
    );
    m.insert(
        "download_file",
        json_schema(vec![
            prop("url", "string", "Public http(s) URL of the file to download (max 100 MiB)."),
            prop("path", "string", "Destination path INSIDE the workspace; must not exist yet (absolute, or relative to the workspace root)."),
        ]),
    );
    m.insert(
        "run_python",
        json_schema(vec![
            prop(
                "code",
                "string",
                "The Python source code to run (a single script).",
            ),
            prop_opt(
                "timeout_secs",
                "integer",
                "Execution timeout in seconds (default 30, max 120).",
            ),
        ]),
    );
    m.insert(
        "run_javascript",
        json_schema(vec![
            prop("code", "string", "The JavaScript/TypeScript source to run (a single module). Deno preferred, Node.js >= 20 fallback."),
            prop_opt("timeout_secs", "integer", "Execution timeout in seconds (default 30, max 120)."),
        ]),
    );
    m.insert(
        "task",
        json_schema(vec![
            prop_opt(
                "subagent_type",
                "string",
                "Specialist profile: `explore` (read-only codebase recon), `implement` (one focused change + verify) or `review` (adversarial read-only review). Defaults to `explore`.",
            ),
            prop(
                "task",
                "string",
                "Single self-contained instruction for the subagent. It cannot see your conversation — include every file path, symbol and detail it needs. Finish yourself with a plain-text summary of its report; do not echo it verbatim.",
            ),
            prop_opt(
                "model_override",
                "string",
                "Optional model override for this subagent: a GGUF file path (local model) or `remote` (use the current remote provider). Enables architect mode — route planning to a large model and implementation to a smaller one.",
            ),
        ]),
    );
    m.insert(
        "git_blame",
        json_schema(vec![
            prop(
                "path",
                "string",
                "File to blame (relative to the workspace root).",
            ),
            prop_opt(
                "start_line",
                "integer",
                "First 1-based line of the range to blame.",
            ),
            prop_opt(
                "end_line",
                "integer",
                "Last 1-based line of the range to blame.",
            ),
        ]),
    );
    m.insert(
        "git_push",
        json_schema(vec![
            prop_opt("remote", "string", "Remote name (default `origin`)."),
            prop_opt(
                "branch",
                "string",
                "Branch to push (default: current branch).",
            ),
            prop_opt(
                "set_upstream",
                "boolean",
                "Push with `-u` to set upstream tracking — use for the first push of a new branch.",
            ),
        ]),
    );
    m.insert("git_pull", json_schema(vec![]));
    m.insert(
        "git_create_branch",
        json_schema(vec![prop(
            "name",
            "string",
            "New branch name; creates it AND switches to it (`git switch -c`). Refuses if it already exists.",
        )]),
    );
    m.insert("git_pr_status", json_schema(vec![]));
    m.insert("git_ci_status", json_schema(vec![]));
    m.insert(
        "create_pr",
        json_schema(vec![
            prop("title", "string", "Pull-request title."),
            prop_opt("body", "string", "Pull-request description (Markdown)."),
        ]),
    );
    m.insert(
        "summarize_changes",
        json_schema(vec![]),
    );
    m.insert(
        "read_lints",
        json_schema(vec![prop(
            "path",
            "string",
            "File to lint (relative or absolute). Reports syntax errors for JS/TS/JSX/TSX, TODO/FIXME/HACK/XXX comment markers for any text file, empty catch blocks and stray `debugger` statements.",
        )]),
    );
    m.insert(
        "view_repo_map",
        json_schema(vec![
            prop_opt(
                "top_n",
                "integer",
                "Number of most-relevant symbols/files to return (default 60, max 300).",
            ),
            prop_opt(
                "root",
                "string",
                "Optional directory to scope the map to (defaults to the workspace root).",
            ),
        ]),
    );
    m.insert(
        "suggest_skills",
        json_schema(vec![
            prop(
                "prompt",
                "string",
                "The task description to match skills against (e.g. the user's request).",
            ),
            prop_opt(
                "path",
                "string",
                "Optional active file path; skills whose globs match it rank first.",
            ),
        ]),
    );
    m.insert(
        "ask_question",
        json_schema(vec![
            prop(
                "question",
                "string",
                "The question to ask the user. Be specific; include context so they can answer without scrolling back.",
            ),
            prop_opt(
                "choices",
                "array",
                "2-4 preset short answers shown as buttons. The user can always type a custom answer instead.",
            ),
        ]),
    );
    m.insert(
        "send_to_user",
        json_schema(vec![prop(
            "message",
            "string",
            "A short message meant for the human user (status notes, decisions taken, heads-ups). Not a substitute for your final report.",
        )]),
    );
    m.insert(
        "tree_sitter_query",
        json_schema(vec![
            prop(
                "path",
                "string",
                "Absolute path to the source file to query (JS/TS/TSX/JSX supported).",
            ),
            prop(
                "query",
                "string",
                "A tree-sitter S-expression query pattern. Use @name captures to extract specific nodes. Example: `(function_declaration name: (identifier) @fn_name)`. Use `_.field` for field access, `(node_type)` for type matching, and alternatives like `(a|b)` for union patterns.",
            ),
            prop_opt(
                "max_results",
                "integer",
                "Maximum number of matches to return (default 50, max 200).",
            ),
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
}
