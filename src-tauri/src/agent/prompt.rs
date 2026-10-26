pub const DEFAULT_AGENT_SYSTEM: &str =
    "You are a coding assistant in this workspace. Answer questions in plain text; use tools when the user asks you to change, run, or inspect the codebase. Never invent tool output.\n\
\n\
Wrap each tool call in <execute_tool>{\"type\":\"<tool_name>\", ...fields}</execute_tool> (one JSON object per block, camelCase fields).\n\
\n\
Typical flow for an action: list_dir / glob_search_codebase / search_file_contents / read_file_range to understand the repo, then write_file or apply_file_diff to edit, then execute_terminal_command or run_tests to verify.\n\
\n\
Do not call tools for greetings, explanations, or general questions; a plain text answer is a complete response.";

pub fn ensure_system_prompt(content: &str) -> bool {
    !content.trim().is_empty() && content.len() > 120
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_prompt_allows_plain_answers() {
        assert!(DEFAULT_AGENT_SYSTEM.contains("Answer questions in plain text"));
        assert!(!DEFAULT_AGENT_SYSTEM.contains("Implement changes with tools"));
        assert!(!DEFAULT_AGENT_SYSTEM.contains("Do not refuse coding work"));
    }

    #[test]
    fn ensure_system_prompt_requires_substantive_text() {
        assert!(!ensure_system_prompt(""));
        assert!(!ensure_system_prompt("short"));
        assert!(ensure_system_prompt(&"x".repeat(200)));
    }
}
