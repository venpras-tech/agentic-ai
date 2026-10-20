# Tauri Commands & Events

Deliverable 1 of `todos.txt`. Generated from the running code
(`src-tauri/src/commands/*`, `main.rs`) — command names are the snake_case
handlers; the UI invokes them via `ui/src/lib/ipc.ts` (`api.*` wrappers).

## Commands

### Workspace / files
- `pick_workspace_folder`, `agent_set_workspace`, `agent_get_workspace`, `agent_get_workspaces`, `agent_add_workspace`, `agent_remove_workspace`
- `list_directory`, `read_text_file`, `write_text_file`, `path_exists`, `revert_file`, `save_file_as`, `save_file_as_bytes`, `pick_text_file`
- `start_file_watcher`, `stop_file_watcher`, `file_watcher_active` — watcher re-indexes changed paths into the workspace index (debounced) and emits `workspace://file-changed`
- `index_status`, `index_rebuild`, `index_search`, `index_semantic_search` *(pluggable `SemanticSearchBackend`; default keyword scorer, `embedder` feature slot)*
- `lsp_diagnostics` *(real language-server bridge — rust-analyzer / typescript-language-server / pyright by extension, config via `lspServers`; returns `LspDiagnostics { server, diagnostics }`. Servers are cached per (server, root); `LspManager::shutdown_all` kills them on workspace change and app exit.)*

### Agent / chat / edit review
- `agent_run_task`, `agent_run_background`, `list_background_tasks`, `abort_background_task`, `background_task_output` *(tails the per-task output ring buffer: streamed assistant tokens + tool/step/done lines, keyed by session; default 200 lines, cap 400; buffer outlives the task so finished tasks stay inspectable)*, `abort_agent_execution`, `agent_steer`
- `agent_execute_tool`, `agent_batch_execute`, `agent_tool_schemas`, `agent_workflows`
- `agent_respond_hitl`, `agent_respond_permission`, `agent_respond_question`
- `agent_list_pending_edits`, `agent_apply_edits`, `agent_reject_edits`, `agent_resolve_edit_conflict` *(conflict edits: `overwrite=true` force-writes the proposal, checkpointing the disk content it replaced; `false` keeps the disk version and rejects. Only the ids passed in are touched — other conflicts in the group stay actionable.)*
- `agent_checkpoint_list`, `agent_checkpoint_restore` *(turn-scoped rewind UI — P5.1: the finished-turn **Rewind** action opens a menu to restore **code + conversation**, **conversation only**, or **code only**. `lib/rewind.ts` picks, per file edited at/after the turn's timestamp, the earliest pre-apply snapshot so one restore per path rolls the file back cleanly; conversation restore reuses `truncateMessages`, code restore calls `agent_checkpoint_restore`. Shell-command changes are not covered.)*, `agent_git_checkpoint_cmd`, `agent_git_checkpoints_cmd`, `agent_git_revert_cmd`
- `agent_run_verify` *(post-apply verify pass: reads `.ai/verify.json` `{ command, cwd?, timeoutSecs? }` or a per-language default (`cargo check` / `npx tsc --noEmit` / `python -m py_compile`), runs it in the workspace, merges LSP diagnostics for the touched files, truncates output to 4000 chars. Returns `VerifyRun { ok, command, exitCode?, report }` and emits `agent://verify-result`. The orchestrator runs the same harness after applying edits, with a 2-pass self-heal cap.)*
- `session_append`, `session_load`, `session_tail`, `session_fork`, `session_delete_chat`, `session_projects`, `console_history`
- `context_status`, `context_set_system_prompt`, `context_set_file_buffer`, `context_set_selection`, `context_push_turn` *(Phase 2)*
- `agent_attach_file`, `agent_detach_file`, `agent_list_attachments` *(session RAG index: `AttachmentsPanel` in the composer lists path/size/chunk count, attaches via `pick_text_file`, detach clears the backend entry)*

### Policy / security / skills *(Phase 3: `agent_policy_save`)*
- `agent_policy_snapshot`, `agent_policy_save`, `agent_set_yolo`, `agent_grant_path`, `agent_revoke_path`, `agent_audit_log` *(P5.3: `agent_policy_save` also accepts `autoApprove` — a per-category map (`read`/`edit`/`shell`/`gitWrite`/`mcp`/`browser`/`other`) that auto-allows matching tools in `policy::check`. Tools in `always_ask` (e.g. `download_file`) and any red-zone (`__red_zone__` synthetic deny rule) bypass the matrix.)*
- `agent_modes`, `workflow_enforce_tools`, `agent_reset_mcp`, `mcp_catalog_load`, `mcp_catalog_save`, `mcp_server_probe` *(spawn/reuse, `tools/list`, cached; ui wrapper: `mcpServerProbe` → `McpProbeResult`. Settings exposes a per-server **Test** button + per-tool enable checkboxes backed by `McpServerConfig.allowedTools`. At call time `call_mcp_tool` refuses disabled servers and tools outside the allow-list (blank = allow all; trailing `*` = prefix match).)*
- `skill_install`, `skill_uninstall`, `skill_install_sample`, `skill_import_dialog`, `skill_set_active`
- `knowledge_scan`, `knowledge_report_cmd`

### Models / inference
- `model_status`, `loaded_model_path`, `load_model_from_path`, `auto_load_model`, `unload_model`, `pick_and_load_model`
- `stream_inference`, `cancel_inference`, `autocomplete_generate`, `voice_transcribe_data` *(`stream_inference` is also the Ask-mode chat-complete path: single turn, zero tool loop, `active_pool` fallback to a routed remote Editor provider; the composer's Settings → Agent "Ask uses chat completion" toggle routes Ask through it instead of `agent_run_task`)*
- `list_remote_models`, `configure_remote_model`, `list_downloaded_models`, `hf_search`, `hf_download_model`, `hf_list_active`, `hf_cancel_download`, `hf_pause_download`, `hf_resume_download`, `delete_downloaded_model`, `models_dir_info`, `open_models_dir`, `local_model_config_save`, `local_model_configs_load`
- `providers_list`, `providers_upsert`, `providers_remove`, `providers_set_role`, `providers_clear_role`, `providers_route`

### Misc / plumbing
- `settings_load`, `settings_save`, `system_info`, `update_check`, `updater_configured`
- `api_server_start`, `api_server_status`, `api_server_stop`
- `terminal_spawn`, `terminal_write`, `terminal_kill`, `terminal_list` *(interactive shell — backend complete; drawer in Phase 3)*
- Smoke: `smoke_active`, `smoke_boot_ok`, `smoke_fail`

## Events

### Engine status (main.rs emitter)
- `inference-token`, `inference-done`, `execution-aborted`, `console-log`

### Model
- `model-loaded`, `model-load-progress`, `model-path`

### Agent channel (`agent://` prefix)
- `agent-step`, `agent-subtask`, `agent-knowledge`
- `agent://tool-event`, `agent://tool-output`, `agent://plan-step`, `agent://todo-update` *(`agent://tool-event` carries an optional `exit_code` (from `ToolResult.stats.exit_code`) surfaced as a live badge in the transcript.)*
- `agent://hitl-gate`, `agent://permission-request`, `agent://question-request`
- `agent://steer-applied`, `agent://terminal-output`
- `agent://bg-task-event`, `agent://context-trimmed`
- `agent://run-trace` *(Phase 1 — per-generation-step `RunTraceEvent`: sessionId, stepIndex, group, tools, durationMs, tokensIn/Out, outcome, timestampMs)*
- `agent://file-changed`, `agent://edit-proposed`, `agent://edit-applied`, `agent://edit-failed`
- `agent://verify-result` *(post-apply verify outcome: `{ sessionId, ok, command, exitCode?, output, timestampMs }`)*
- `agent://subagent-run` *(first-class `task` subagent lifecycle: `{ parentSessionId, childSessionId?, profile, status: "started" | "completed" | "error", summary?, startedAt, durationMs?, leasedWorkers? }`, emitted at child start and end. Nesting capped by `MAX_SUBAGENT_DEPTH` (2); a busy engine pool returns a structured "pool busy" tool error.)*
- `agent://hook` *(P3.1 workspace hooks: `{ event: "pre_tool_use" | "post_tool_use" | "session_start", tool?, status, detail?, sessionId }`. Emitted on a `pre_tool_use` deny and when `session_start` captures stdout. Configured in `.ai/hooks.json` — see `manual-test-script.md` step 24 for the schema and a working example.)*
- `agent://skills-changed` *(P3.4: empty payload; emitted by `skill_set_active`, `skill_install`, `skill_import_dialog`, `skill_install_sample`, `skill_uninstall`. `useAgentStream` re-dispatches a `vencoder-skills-changed` DOM event that refreshes the Settings → Skills list.)*

### Workspace
- `workspace://file-changed`
- `index://progress` — payload `{ phase: "start" | "done", status?: IndexStatus }`; emitted by `index_rebuild`, consumed by `subscribeEngineEvents` (`onIndexProgress`) to drive the header `IndexStatusChip`.

### Frontend DOM events (renderer-internal, not Tauri)
- `vencoder-open-editor` (detail: absolute path string) — opens `/editor?path=…`
- `vencoder-prefill-composer` (detail: prompt string) — pre-fills the chat composer
- `vencoder-insert-composer` (detail: text string) — appends to the chat composer draft (used by the Terminal drawer's **Mention** action)
- `vencoder-send-prompt` (detail: text string) — submits a prompt (used by Retry/Regenerate on a turn)
- `vencoder-edit-request` (detail: `{ index, text }`) — loads a prior user message back into the composer for editing
- `vencoder-focus-composer` — focuses the composer input
- `vencoder-undo-turn` — P5.1: reverts the last exchange (restores code + truncates the conversation). Dispatched by the `/undo` and `/rewind` slash commands and by double-`Esc` in the transcript.
- `vencoder-skills-changed` — dispatched by `useAgentStream` on `agent://skills-changed`; refreshes the Settings → Skills list

## Subscribe in the UI
- Engine events: `subscribeEngineEvents` (`ui/src/lib/ipc.ts`) — returns an unsubscribe fn.
- Terminal output: `subscribeTerminalOutput` → `agent://terminal-output` payload `{ id, data, stream: "stdout" | "stderr" | "exit", exitCode }`.