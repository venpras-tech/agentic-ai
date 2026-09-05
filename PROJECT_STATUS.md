# AI Code Editor — Project Status

**Last Updated**: 2026-09-05 (model-boundary typing, dedicated Debug profile, row-by-row subtask status, queued mid-run messages, App.tsx reducer-store consolidation, `/fork-as-background`, token-fan-out caps, multi-model subagents, bundled tokenizer)
**Stack**: Tauri 2 (Rust) + React 19 + Vite 6 + Tailwind v4 + Monaco editor
**Inference**: llama.cpp GGUF (local) + OpenAI-compatible SSE (remote), multi-provider routing (Planner/Editor/Autocomplete/Embed roles)
**Agent**: ReAct loop with 48+ tools, 4 subagent profiles (explore/implement/review/debug), plan mode, decompose mode, background tasks, custom modes, auto-memory extraction, multi-stage context compaction, repo-map (PageRank), skill auto-suggest

---

## P0 — Critical (Security / Correctness / Major Performance)

### Legacy P0 (All Complete)

- [x] **Unsafe transmute in background tasks** (`src-tauri/src/agent/background.rs:52`)
  - Status: Already fixed — uses `Arc<ToolState>`.

- [x] **No per-tool execution timeout** (`src-tauri/src/agent/tools.rs:55-417`)
  - Status: Already implemented — `TOOL_EXECUTION_TIMEOUT = 60s`.

- [x] **Semantic search rebuilds entire index on every call** (`src-tauri/src/agent/tools.rs:1148-1357`)
  - Status: Fixed — cached in `ToolState::sem_index`.

- [x] **No KV-cache reuse across agent steps** (`src-tauri/src/agent/orchestrator.rs:801-819`)
  - Status: Fixed — `InferenceRequest.cached_prefix_tokens` + orchestrator tracking.

- [x] **`view_file_structure` hardcoded to TypeScript** (`src-tauri/src/agent/tools.rs:1384`)
  - Status: Fixed — `resolve_grammar()` supports JS/TS/TSX/JSX, Python, JSON, Rust.

### Audit P0 — Bugs & Performance (New)

- [x] **`closeFile` calls `setActiveKey` inside `setFiles` updater** (`src/App.tsx:958`)
  - Status: Fixed — moved `setActiveKey` to `queueMicrotask()` to avoid setState-in-updater.

- [x] **`syncAgentFile` depends on `files`, causing full event re-subscription** (`src/App.tsx:178`)
  - Status: Fixed — added `filesRef` pattern, `syncAgentFile` now has empty deps `[]`.

- [x] **`parseEvent` unsafe cast + `JSON.parse` can throw** (`src/lib/events.ts:77`)
  - Status: Fixed — wrapped in try/catch, returns `{}` on malformed payloads, validates non-string objects.

- [x] **No message list virtualization** (`src/components/ChatPanel.tsx:874`)
  - Status: Fixed — ChatPanel already renders the list through `react-virtuoso` `<Virtuoso>` (with `VirtuosoHandle`) for large conversations; verified this session.

- [x] **`buildTurnSegments` runs O(n log n) per turn per render** (`src/components/ChatPanel.tsx:430`)
  - Status: Fixed — wrapped in `useMemo` keyed on `[isError, message, liveText]`. `turnText` also memoized.

- [x] **`openFilePicker` missing from keyboard shortcut effect deps** (`src/App.tsx:1015`)
  - Status: Fixed — moved `openFilePicker` before the effect, added to dependency array.

### P0 — Agent Gaps (New)

- [x] **Skills active flags not persisted** (`src-tauri/src/agent/skills.rs`)
  - Competitors: Claude Code/Cursor/Roo all persist rules activation state
  - We have: Active flags are in-memory only — lost on restart, re-activates everything on next scan
  - Status: Fixed — `save_active_state()` / `load_active_state()` write to `.ai/skills-state.json` on every `set_active()` call and restore on `scan()`. Verified in-session.

- [x] **Session permissions not persisted** (`src-tauri/src/agent/mod.rs:880`)
  - Competitors: All major tools persist permission decisions across sessions
  - We have: `session_allow` HashSet is pure in-memory — users re-approve tools every session
  - Status: Fixed — `save_session_allow()` / `load_session_allow()` persist to `.ai/session-permissions.json` on every `allow_session` decision and load on ToolState/orchestrator setup. Verified in-session.

- [x] **No `/bug` command for structured bug analysis** (`src/components/ChatPanel.tsx`)
  - Competitors: Roo Debug mode, Cline browser validation, community bug pipelines (bug-report-agent-skills)
  - We have: No structured bug workflow — users paste stack traces into chat manually
  - Status: Fixed — `/bug [description]` slash command (ChatPanel.tsx:736) + `analyze_bug` tool (tools.rs). Parses stack traces (file:line extraction), searches related code, ranks candidate root causes, proposes fixes. Verified in-session.

- [x] **No `/review` command for code review** (`src/components/ChatPanel.tsx`)
  - Competitors: Claude Code `/code-review` with --fix flag, Aider `--review` mode, Roo Review mode
  - We have: No structured review workflow
  - Status: Fixed — `/review [file/PR/diff]` slash command (ChatPanel.tsx:745) + `review_code` tool (tools.rs). Read-only analysis for correctness/concurrency/security/error-handling/style with severity ranking. Verified in-session.

- [x] **No auto-memory extraction from conversations** (`src-tauri/src/agent/skills.rs`)
  - Competitors: Claude Code auto-memory (4-level hierarchy, per-topic files), Windsurf auto-Memories, Cursor Memories
  - We have: Manual skill creation only — no cross-session learning
  - Status: Fixed — after each *successfully completed* coding task, `maybe_extract_memory` (orchestrator.rs:2106) runs a bounded no-tool LLM pass over the conversation tail, distills up to 4 durable learnings (file locations, conventions, decisions, gotchas) and appends them to `.ai/memory.md` via `KnowledgeState::append_memory`. `scan()`/`memory_content` load the notes back into the model context on the next session. Notes are user-editable and capped at 200 lines (oldest dropped first).

- [x] **No context compaction pipeline** (`src-tauri/src/agent/context.rs`)
  - Competitors: Claude Code 5-tier cascade (budget → snip → microcompact → context collapse → auto-compact), Cline/Roo auto-compact
  - We have: `trim_working_history` only (chars/4 heuristic, drops oldest)
  - Status: Fixed — `compact_context()` multi-stage pipeline is **wired into the live agent loop**: `trim_working_history` (orchestrator.rs:2280) first compresses oversized messages (tool outputs / long replies / non-system pinned buffers) around a head+tail marker (`compress_large_messages`), then evicts oldest non-pinned messages. **Stage 3 (LLM summarization) also shipped this session**: `summarize_old_block` (orchestrator.rs) replaces the oldest unpinned block (≤6 msgs / ≤12k chars, `oldest_unpinned_block`) with one pinned system message `## Summarized history (N earlier messages)` via an 800-token no-tool generation, bounded by `MAX_SUMMARIZATIONS=2` per run; wired into `run_focused_steps` (over-budget before trim), `plan_subtasks` (pre-decompose) and `run_summary` (pre-summary). Pinned messages (system/usermemo) are never collapsed. Tests: `oldest_unpinned_block_*`, `summarize_old_block_inlines_pinned_summary_in_place`.

---

## P1 — Important (Feature Gaps vs Competitors)

### Legacy P1 (All Complete)

- [x] **No Markdown rendering in chat** — Fixed
- [x] **No inline editor diffs with accept/reject** — Fixed
- [x] **No Escape key to interrupt agent** — Fixed
- [x] **No undo/redo in chat composer** — Fixed
- [x] **No retry logic in `tauriInvoke`** — Fixed
- [x] **Brittle refusal detection** — Fixed
- [x] **`is_coding_task()` too broad** — Fixed
- [x] **No multi-file transactional edits** — Fixed
- [x] **No multi-root workspace support** — Fixed
- [x] **No component-level frontend tests** — Fixed (53 tests)
- [x] **No context window pressure indicator** — Fixed
- [x] **No warning when context is aggressively trimmed** — Fixed
- [x] **No request deduplication in IPC** — Fixed

### Audit P1 — UX & Performance (New)

- [x] **Monaco `setValue()` destroys undo history on file switch** (`src/components/EditorPane.tsx:67`)
  - Status: Fixed — model-based file management (`createModel`/`setModel`/`pushEditOperations`, EditorPane.tsx:84-107). One model per path, reused on revisit; `setValue()` is never called. Undo history preserved.
- [x] **ModelBar dropdowns have no click-outside or Escape dismissal** (`src/components/ModelBar.tsx:285`)
  - Status: Fixed — document `mousedown`/`touchstart` click-outside + `Escape` key handler close both dropdowns (ModelBar.tsx:168-190).
- [x] **DiffView has no virtualization for large diffs** (`src/components/DiffView.tsx:107`)
  - Status: Fixed — hand-rolled windowed rendering (`ROW_HEIGHT`/`OVERSCAN` + `slice`), DiffView.tsx:37-75,153-163.
- [x] **Remote model list fetch has no request cancellation** (`src/components/ModelBar.tsx:188`)
  - Status: Fixed — monotonic sequence counter drops stale responses (`fetchSeqRef`, ModelBar.tsx:163,220-239).
- [x] **Auto-scroll ignores user scroll position** (`src/components/ChatPanel.tsx:612`)
  - Status: Fixed — tracked via `onAtBottom`/`isAtBottom`; auto-scroll only fires when the user is at the bottom (ChatPanel.tsx:664-668,959). Fixed in this session.
- [x] **`parseUnifiedDiff` runs on every DiffView render** (`src/components/DiffView.tsx:48`)
  - Status: Fixed — wrapped in `useMemo` keyed on `[diff]` (DiffView.tsx:52-61).
- [x] **Monaco options object recreated every render** (`src/components/EditorPane.tsx:70`)
  - Status: Fixed — `EDITOR_OPTIONS` is a module-level constant (EditorPane.tsx:42-62).
- [x] **12+ inline JSX callbacks in App.tsx defeat child memoization** (`src/App.tsx:1435-1674`)
  - `onParamsChange`, `onYoloChange`, `onAttachClick`, `onDetachFile`, `onDropFiles`, `skills` array — all unstable
  - Fix: Extract to `useCallback`, pass stable refs
  - Status: **Shipped** — `handleParamsChange` (App.tsx:1649), `handleYoloChange` (1653), `handleAttachClick` (1658), `handleDetachFile` (1673), `handleDropFiles` (1678) all memoized with `useCallback`.
- [x] **`sessionAppend` retry can duplicate JSONL records** (`src/lib/ipc.ts:289`)
  - Status: Fixed — every turn now carries a client-generated `turnId` (both user and assistant/error halves), and backend `session_append` no-ops replayed turns via `log_has_turn_id` (main.rs + test). Writes are now replay-safe, so the "never retries" mitigation is complemented by true idempotency.
- [x] **`Promise.all` on event listeners leaks on partial failure** (`src/lib/ipc.ts:307`)
  - Status: Fixed — uses `Promise.allSettled` and unsubscribes surviving listeners before rethrowing on any rejection (ipc.ts:328-402).

### P1 — Agent Gaps (New)

- [x] **No per-directory rules hierarchy** (`src-tauri/src/agent/skills.rs`)
  - Status: Fixed — depth-bounded recursive `find_nested_agents_md` (skills.rs:623-656, `NESTED_RULES_DEPTH=8`) appends each nested `AGENTS.md` as an ordered `### From <rel>` section after root rules; skipped dirs (node_modules/.git/target/dist/.ai) excluded (skills.rs:612-617). Wired into context via `sync_knowledge` → `upsert_pinned("rules", …)` (main.rs:1682-1696) → every agent prompt. Tests: `nested_agents_md_*`.

- [x] **Skill categories/tags + auto-suggest** (`src-tauri/src/agent/skills.rs`)
  - Status: Fixed — `tags:` and `globs:` frontmatter fields parsed into `Skill` (skills.rs:43-47,576-579); `tags()` returns the sorted union for UI filtering (skills.rs:395-404). **Auto-suggest added in this session**: new `suggest_skills` tool (tools.rs) calls `KnowledgeState::suggest(prompt, path)` (skills.rs) which ranks skills by glob-match against the active file (+100) plus keyword overlap; advertised in `core.rs` schema, policy read-only allow list, subagent read-only tools, and `prompt.ts`. Tests: `suggest_ranks_glob_hit_above_keyword_only`, `glob_match_*`.

- [x] **No auto-verify after code edits** (`src-tauri/src/agent/orchestrator.rs`)
  - Status: Fixed — gated behind `request.verify` (orchestrator.rs:153-156): after each step with edits, up to `MAX_VERIFY_PASSES=2` background verify passes run `cargo check`/`npx tsc --noEmit`/`python -m py_compile` and inject the report back (orchestrator.rs:1180-1221, 2396-2479). Wired from the frontend (main.rs:1115-1120).

- [x] **No repo-map via symbol graph** (`src-tauri/src/agent/tools.rs`)
  - Status: Fixed — `RepoGraph` defs/incoming/outgoing (tools.rs:1575-1582) + deterministic PageRank (tools.rs:1708-1739), mtime-keyed cache (tools.rs:1747-1924), `view_repo_map` tool (tools.rs:1928-1983, dispatch 458-460). **Advertised to the model in this session**: added `view_repo_map` schema to `core.rs` tool_schemas, policy read-only allow list, subagent read-only tools, and `prompt.ts`.

- [x] **No multi-provider model routing** (`src-tauri/src/engine.rs`)
  - Status: **Wired in this session** — `ProviderRegistry` (remote.rs:911-1005) with `ProviderRole` (Planner/Editor/Autocomplete/Embed) and `route()` fallback is now registered in Tauri managed state (`ProviderRegistryState`, main.rs:58-69) with six `providers_upsert/remove/set_role/clear_role/route/list` commands (main.rs ~547-600) exposed via IPC (`src/lib/ipc.ts`) and `src/types.ts`. Integrated into `configure_remote_model` (main.rs:484-513): an Editor-routed non-Local provider overrides the caller's config; an empty registry preserves today's single remote behavior exactly. 5 new command tests. Full per-role pool execution deferred as follow-up.

- [x] **No named checkpoints/snapshots** (`src-tauri/src/main.rs`)
  - Status: Fixed — user-named checkpoints persisted to `.ai/checkpoints.json` (main.rs:1930-1962); `agent_git_checkpoint_cmd` records optional names (1997-2029), `agent_checkpoint_names_cmd` browses them (2079-2088); auto-checkpoint before each file edit (tools.rs:216-232); `CheckpointMenu.tsx` save/list/revert UI.

---

## P2 — Nice-to-Have

### Legacy P2 (All Complete)

- [x] **No model selection from multiple loaded models** — Fixed
- [x] **No drag-and-drop file upload** — Fixed
- [x] **No cost estimation / budget tracking** — Fixed
- [x] **No Aider-style architect mode** — Fixed
- [x] **No session fork / conversation branching** — Fixed
- [x] **No tree-sitter query tool** — Fixed
- [x] **No loading/skeleton states for file explorer** — Fixed
- [x] **No proactive file change notification** — Fixed
- [x] **No git change summary tool** — Fixed
- [x] **No `.gitignore`-aware file watching** — Fixed

### Audit P2 — Correctness & Polish (New)

- [x] **Escape only works during streaming — no Escape for modals** (`src/App.tsx:1037`)
  - Users expect Escape to close Settings, Knowledge, Permission dialogs
  - Fix: Add Escape handler that closes open modal
  - Status: Fixed this session — App keydown handler closes Settings/Knowledge/Permission/Question dialogs (and aborts streaming), plus per-modal handlers in SettingsModal, KnowledgePanel, PermissionModal.

- [x] **No ARIA landmarks or roles** (`src/App.tsx:1415`, `src/components/ChatPanel.tsx:861`)
  - No `<main>`, `<nav>`, `<aside>`, `role="log"`, `aria-live="polite"` on message list
  - Fix: Add semantic landmarks and ARIA attributes
  - Status: Fixed this session — landmarks already present (`<nav aria-label="Sidebar">`, `<main>`, ChatPanel `<aside>`); added `role="log"` + `aria-live="polite"` + `aria-label` to the message list and `aria-label` on the chat aside.

- [x] **Debounce settings persistence** (`src/App.tsx:696-717`)
  - Read-modify-write cycle fires on every workspace/chat/params change; race conditions on rapid updates
  - Fix: Debounce 500ms, or write directly to a ref with flush-on-unmount
  - Status: Fixed this session (verified) — `debouncedSaveSettings` (500ms) with flush-on-unmount already in place.

- [x] **DiffView reject silently swallows errors** (`src/components/DiffView.tsx:58`)
  - `revertFile` failure gives no feedback; user thinks revert worked
  - Fix: Show error toast or inline error message on failure
  - Status: Fixed this session — inline `role="alert"` banner on revert failure; `parseUnifiedDiff` memoized.

- [x] **`fileChangeNotice` setTimeout not cleaned up on unmount** (`src/App.tsx:554`)
  - Multiple timeouts created on rapid changes; never cleared on unmount
  - Fix: Store timeout ID in ref, clear in useEffect cleanup
  - Status: Fixed (verified this session) — `fileChangeTimerRef` (App.tsx:168) reused across rapid changes and cleared in the setup effect's unmount (App.tsx:624-628).

- [x] **Synchronous `std::fs` in async Tauri commands** (`src-tauri/src/main.rs:655,1229,2124`)
  - `persist_model_path`, `list_directory`, `session_append` block Tokio runtime
  - Fix: Replace with `tokio::fs` or `spawn_blocking`
  - Status: Fixed (verified this session) — `persist_model_path` and `session_append` now use `tokio::fs`; `session_projects` and its helpers (`chat_title`/`count_lines`/`modified_ms`) already run inside `tokio::task::spawn_blocking`. No sync fs remains on the async runtime.

- [x] **Backend commands return `serde_json::Value` instead of typed structs** (`src-tauri/src/main.rs:1722,1741,1814`)
  - Audit, policy, checkpoints return untyped JSON; structural drift invisible to compiler
  - Fix: Define typed Rust structs, derive Serialize, use in command return types
  - Status: Fixed (verified this session) — audit/policy/checkpoints commands now return typed structs (`AuditEntry` vec, `PolicySnapshot`, `CheckpointInfo`). Remaining `Value` uses are the tool-schema map (dynamic by nature) and event payloads (hf-download-progress).

- [x] **Settings schema completely untyped** (`src/lib/ipc.ts:286`)
  - `settingsLoad` returns `Record<string, unknown>`; every consumer defensively casts
  - Fix: Define `AppSettings` struct in Rust, generate TypeScript type
  - Status: Fixed this session — `settingsLoad`/`settingsSave` are typed on the `AppSettingsRecord` interface (src/types.ts) mirroring the Rust `AppSettings` struct (camelCase, optional fields, catch-all); App.tsx consumers dropped their defensive casts.

- [x] **`chatStatus` subtask done clears label unconditionally** (`src/lib/chatStatus.ts:181`)
  - If a tool started between subtask "running" and "done" events, the tool's label is clobbered
  - Fix: Only clear label if it belongs to the finishing subtask
  - Status: Fixed (verified this session) — the reducer only clears when `state.label === \`subtask {index}/{total} · {title}\``, so an interleaved tool label is preserved.

- [x] **`agent://context-trimmed` event emitted but not subscribed** (`src/lib/events.ts`)
  - Backend emits when history trimmed >50%; frontend ignores it
  - Fix: Add handler, show notification in UI
  - Status: Fixed (verified this session) — App.tsx:556-567 subscribes `EVT_CONTEXT_TRIMMED` and shows a 6s auto-dismissing notice banner.

- [x] **Dead `AgentToolEvent.output` field** (`src/types.ts:64`)
  - Field exists in TypeScript but is never emitted by backend
  - Fix: Remove field, or wire it up in backend
  - Status: Resolved this session — the field is live: the frontend accumulates `execute_terminal_command`/`run_tests` output into it via `chatReducer.appendToolOutput`. Corrected the misleading `@deprecated` doc comment.

- [x] **Four discriminated unions typed as bare `string`** (`src/types.ts:137,159,367,379`)
  - `InferenceDone.outcome`, `AuditEntry.decision`, `BackgroundTaskInfo.status`, `BackgroundTaskEvent.status`
  - Fix: Replace with `"a" | "b" | "c"` union types
  - Status: Fixed (verified this session) — all four are already literal unions (`InferenceDone.outcome` types.ts:176, `AuditEntry.decision` :197-205, `BackgroundTaskInfo.status` :414, `BackgroundTaskEvent.status` :425).

- [x] **`AgentToolEvent.sessionId` optional in TS but always present in Rust** (`src/types.ts:66`)
  - Unnecessary null-checks downstream
  - Fix: Remove `?` from type definition
  - Status: Fixed (verified this session) — `sessionId: number` is required (types.ts:104).

### P2 — Agent Gaps (New)

- [x] **No custom agent modes** (`src-tauri/src/agent/subagent.rs`)
  - Competitors: Roo Code `.roomodes` (YAML-defined modes with tool restrictions), Claude Code custom agents in `.claude/agents/`
  - We have: Fixed 3 subagent profiles (explore/implement/review)
  - Status: Fixed — custom modes load from `.ai/modes/*.md` (frontmatter: `name`, `description`, `allowedTools`, `allowedGlobs`, `modelOverride`; body = system prompt). `load_modes`/`register_modes` feed a thread-local registry; `tool_allowed(profile, tool)` enforces the allowlist (empty list = unrestricted; unknown name = denied). Shipped end-to-end this session: `agent_modes` IPC command + `sync_modes` wired into `agent_set_workspace`/`agent_add_workspace` (main.rs), request carries `agent_mode`, `run_focused_steps` short-circuits disallowed dispatches to a denial ToolResult (orchestrator.rs), `Mode` is now `serde::Serialize` (camelCase), frontend loads modes per workspace and renders a mode dropdown in the chat header that swaps the system prompt (`## Active mode` section). Tests: `custom_mode_tools_enforced_through_tool_allowed`, `load_custom_modes_from_modes_dir`.

- [x] **No user-defined workflows/recipes** (`src-tauri/src/agent/workflows.rs`, `src/components/ChatPanel.tsx`)
  - Competitors: Windsurf `.windsurf/workflows/` (reusable agentic recipes), Continue.dev configurable slash commands
  - We have: Hardcoded slash commands (/plan, /bg, /clear, etc.)
  - Status: **Shipped 2026-09-02** — `workflows.rs` loads `.ai/workflows/*.md` (frontmatter `name`/`description`/`allowedTools` + body directive), exposed via `agent_workflows`/`workflow_enforce_tools` IPC. Frontend loads workflows per workspace, lists them in the `/` hint menu, and dispatches `/name <goal>` → `onWorkflowInvoke` builds the directive+goal prompt and enforces `allowedTools:` via `register_workflow_tools` (thread-local gate reusing the custom-mode enforcement path, `workflow_child_tool_verdict`). Built-in hints remain. Tests: `workflows::*` (4).

- [x] **No browser automation** (`src-tauri/src/agent/tools.rs`)
  - Competitors: Cline Puppeteer (navigate, click, type, screenshot), Cursor Browser tool
  - We have: No browser integration
  - Fix: Add `browse_web` tool using headless Chromium — navigate, click, type, screenshot, extract text. Enable visual bug fixing and E2E testing workflows.
  - Status: **Shipped** — `browse_web` tool (tools.rs:5872, dispatch at tools.rs:449) fetches URLs via `reqwest` with SSRF guard (blocks `localhost`/private IPs); registered in the tool registry (registry.rs:668), policy read-only allow list, subagent read-only tools, and `prompt.ts`. Tests: `browse_web_blocks_private_ips`.

- [x] **No session resume** (`src-tauri/src/main.rs`)
  - Competitors: Claude Code `--continue`/`--resume`, Aider `/restore-session`/`--restore-chat-history`
  - We have: Sessions stored as JSONL but no resume capability
  - Status: **Shipped** — `session_load` replayed on workspace open (Phase 6.4) + `SessionResumePanel` keyboard-driven picker (2026-09-02).

- [x] **No LLM-based retrieval reranking** (`src-tauri/src/agent/tools.rs`)
  - Competitors: Windsurf M-Query, Cursor reranking layer
  - We have: TF-IDF cosine similarity (no reranking)
  - Fix: After initial TF-IDF retrieval, use LLM to rerank top-K results by relevance to query. Reduces noise in semantic search results.
  - Status: **Shipped 2026-09-04** — `SemChunk` now retains the raw window `text`; `semantic_search_codebase` feeds the top-25 cosine matches through the previously-orphaned `rerank_results` (query-overlap reorder with deterministic tiebreak; `_llm_hint` param reserved for a future model-based reranker) before slicing top-k.

---

## P3 — Polish

- [x] **No input length limit on chat textarea** (`src/components/ChatPanel.tsx:1018`)
  - Status: Fixed — `MAX_INPUT_CHARS` + `maxLength` + guard in `submit()` return false on oversize.
- [x] **No unsaved-changes warning on file close** (`src/App.tsx:958`)
  - Status: Fixed — `closeFile` now confirms (window.confirm) when `f.saved` is false.
- [x] **No "Accept All" / "Reject All" for multi-file diffs** (`src/components/ChatPanel.tsx:528`)
  - Status: Fixed — bulk Accept All / Reject All render when >1 diff pending; shared `diffResolved` reducer keeps the Changes panel in sync.
- [x] **No loading indicators for workspace/session switching** (`src/App.tsx:1073,1052`)
  - Status: Fixed — `isSwitching` overlay now shows an animated spinner + "Switching…" label (2026-09-02).
- [x] **`unloadModel` destroys chat view without confirmation** (`src/App.tsx:757`)
  - Status: Fixed — `window.confirm` before destroying the visible conversation.
- [x] **No `aria-label` on icon buttons** (Multiple components)
  - Status: Fixed — added accessible names to App sidebar, FileExplorer, ChatPanel (edit/cancel/attach/export/dictate), ProjectsPanel, ModelBar, DiffView accept/reject, Tabs close.
- [x] **`useTokenStream` RAF not cancelled on unmount** (`src/hooks/useTokenStream.ts:30`)
  - Status: Fixed — RAF id stored + `cancelAnimationFrame` in unmount cleanup.
- [x] **No syntax highlighting in DiffView** (`src/components/DiffView.tsx`)
  - Status: Fixed — `DiffText`/`tokenizeContent` language-aware tokenizer (js/py/rs) for keyword/string/number/comment coloring, driven by `langFromPath`.
- [x] **Consolidate App.tsx state** (`src/App.tsx:70-148`)
  - Status: **Shipped 2026-09-05** — chat transcript (`transcriptStore`), execution DAG (`execGraphStore`) and turn lifecycle (`chatStatusStore`) are now reducer-hosting Zustand stores; the busy queue lives in `agentRunStore` (`enqueuePrompt`/`shiftPrompt`, tested); remaining UI-mutation `useState` slices intentionally stay in App as a prop-feeding orchestrator. 113 frontend tests passing.

---

## Agentic Modes Gap Analysis (Research 2026-08-31)

Comparison of the agentic **modes of operation** our app exposes against the
industry taxonomy (Claude Code, Cursor, Windsurf/Devin, GitHub Copilot, Zed,
OpenCode, Aider, Gemini CLI, Roo Code, Continue, Cline).

### Our current modes (audited)

| Mode | Trigger | Notes |
|------|---------|-------|
| Agent (primary on/off) | Header "Agent" toggle (ChatPanel.tsx:911, App.tsx:175,1321) | Agent loop vs plain chat |
| Plan | `/plan` (ChatPanel.tsx:726, orchestrator.rs:370-422) | Read-only plan, approval gate |
| Act/Execute | `/act` (ChatPanel.tsx:732) | Full tool loop |
| Decompose | `/decompose` (ChatPanel.tsx:736, orchestrator.rs:426-500) | Subtask split + parallel |
| Subagent profiles | `task` tool (subagent.rs:36-61) | explore / implement / review |
| Custom modes | Header dropdown, `.ai/modes/*.md` (subagent.rs) | name/desc/prompt/allow-list/glob/model |
| Verify | Header toggle / flags (orchestrator.rs:1283-1321) | background lint/typecheck |
| YOLO | Header toggle (policy.rs:184-191) | auto-approve routine cmds |
| Architect (partial) | `task` model_override + ProviderRole Planner (engine.rs:485-506) | Routing at plan phase only |
| Background | `/bg` (App.tsx:1291-1319) | independent agent task |
| Smalltalk | `smalltalk.rs` | **dead code — not wired** |

### Industry taxonomy (12 archetypes)

1. Ask/Q&A (read-only), 2. Plan (read-only + plan output), 3. Build/Execute,
4. Code-Edit-Only (no terminal), 5. Debug (runtime-evidence), 6. Dual-model
Architect, 7. Auto/Turbo/YOLO, 8. Orchestrator (decompose+dispatch), 9. Custom
user-defined modes, 10. Autocomplete/Edit-prediction, 11. Cloud/Background
async agent, 12. Review.

### Mode gap matrix — where we miss vs the industry

| Archetype | Our status | Gap |
|-----------|-----------|-----|
| Ask/Q&A (read-only) | ✅ | `builtin_modes()` in subagent.rs exposes `ask` mode (read-only tools only); enforced on parent loop via `tool_allowed`. |
| Plan | ✅ `/plan` | parity |
| Build/Execute | ✅ `/act` | parity |
| Code-Edit-Only | ✅ | `edit` builtin mode (subagent.rs:306-351), no terminal tools, explicit "CODE-EDIT-ONLY mode" system prompt. |
| Debug | ✅ | `debug` dedicated profile in `PROFILES` (hypothesis → evidence → root-cause prompt, max_steps 6) + `/bug` + `analyze_bug` tool (tools.rs) + runtime-evidence read-only tool set (2026-09-05). |
| Dual-model Architect | ✅ | `model_override` threaded + `RuntimeRouter` now resolves `ProviderRole::Editor` for the flat execute loop, so a separate editor provider (vs the planner) can drive edits (2026-09-04). Custom-mode `allowedGlobs`/`modelOverride` still parsed-not-full-UI-president. |
| Auto/Turbo/YOLO | ✅ YOLO | parity for the permission axis; no "Auto = model routing" layer |
| Orchestrator | ✅ `/decompose` | parity (Roo-only elsewhere) |
| Custom modes | ✅ `.ai/modes/*.md` | close to Roo `.roomodes` / Copilot `.agent.md`; missing `fileRegex`/glob enforcement and handoffs |
| Autocomplete/Edit-prediction | ✅ | `InlineCompletionsProvider` in EditorPane.tsx wired to `Autocomplete` ProviderRole via `api.autocomplete` → `autocomplete_generate`. |
| Cloud/Background | ✅ `/bg` | has local background; no cloud but not required (local-first) |
| Review | ✅ `/review` | parity |
| Mode **handoffs** | ❌ | Copilot `.agent.md` handoffs (Plan→Implement→Review) absent |
| Smalltalk short-circuit | ✅ | Wired into `stream_inference` + `should_shortcut_smalltalk` orchestrator (2026-09-03). |

### Recommended mode-gap todos (ranked)

- [x] **Wire the dead `smalltalk` module** into `stream_inference` + orchestrator (skip generation for trivial greetings) — quick win.
- [x] **Add a first-class Ask (read-only) mode** toggle/profile on the main loop — the single biggest taxonomy gap (present in every competitor).
- [x] **Add a dedicated Debug mode** profile (hypothesis + runtime evidence + logging injection) beyond `/bug` — shipped 2026-09-05 (`debug` profile in subagent.rs `PROFILES`, recognized as built-in, delegatable via `task`, read-only + diagnostics tool set).
- [x] **Implement user-defined workflows** (`.ai/workflows/*.md` → `/workflow-name`), incl. Copilot-style **handoffs** (Plan→Implement→Review) — the explicit open P2.
- [x] **Enforce custom-mode `allowedGlobs` + parent-loop `modelOverride`** (currently parsed, not used) and add a Code-Edit-Only profile.
- [x] **Implement true dual-model Architect** (reasoning model plans, editor model executes) via the existing ProviderRole `Editor` role (currently only Planner is routed at runtime). — **Done 2026-09-04** (flat execute loop routes `ProviderRole::Editor`).

---

## Competitive UI/UX Gap Analysis (Research 2026-08-31)

Comparison of our agentic editor UI against the leading AI coding tools
(Cursor, Windsurf/Devin, GitHub Copilot Agent, Zed, Claude Code, OpenCode,
Aider, Gemini CLI, Continue, v0). The **backend is our strength** (60+ tools,
subagents, MCP, RAG, plan/todo, git, multi-provider routing, HF hub, local API
server) — far more capable than the UI currently exposes. The gaps below are
ranked by expected user-experience impact.

### High-impact UX gaps (first to close)

- [x] **No command palette / quick-open** (Cursor Cmd+Shift+P, Claude Code Ctrl+B/T, Zed Ctrl+T)
  - Status: **Shipped 2026-09-02** — hand-rolled `CommandPalette` (`cmdk`-style, no dep) wired at App.tsx; Ctrl+P opens files, Ctrl+Shift+P opens commands/tools/sessions. See ROADMAP note.
  - Remaining: adopt real `cmdk` for fuzzy ranking/grouping polish if desired.
- [x] **No image / vision input in chat** (Copilot images+PDFs, Gemini multimodal, v0 screenshot-to-code)
  - Status: **Shipped 2026-09-04** — image paste → base64 attachment surfaced to vision-capable remote providers. Frontend: ChatPanel textarea `onPaste` reads image files → `ImageAttachment[]` (base64 data URLs) with thumbnail previews + per-image remove; `SendOptions.images` carries them through `sendPrompt` → `streamInference`/`agentRunTask` (App.tsx). Backend: `InferenceRequest.images`/`ImageAttachment` (engine.rs), `user_content_value()`/`split_data_url()` emit OpenAI `image_url` blocks and Anthropic `image` base64 blocks (remote.rs), plumbed through the orchestrator parent loop (`AgentTaskRequest.images`). Local llama.cpp has no vision and ignores images.
- [x] **No integrated interactive terminal** (Cursor, Claude Code Desktop, Devin for Terminal)
  - Status: **Shipped** — `TerminalPanel.tsx` (xterm.js-backed PTY pane), `terminal_spawn`/`terminal_write`/`terminal_kill`/`terminal_list` commands (main.rs:1626-1656), wired into App.tsx (Ctrl+Alt+T). Shares backend shell tool via IPC.
- [x] **No in-editor AI completion / autocomplete surface** (Cursor Supermaven, Copilot NES, Zed Zeta)
  - Status: **Shipped** — `ensureInlineProvider` (EditorPane.tsx:105) registers a Monaco `InlineCompletionsProvider` that calls `api.autocomplete` → backend `autocomplete_generate` (main.rs:1484). Wired to the `Autocomplete` ProviderRole.
- [x] **No multi-file source-control / changes panel** (Cursor, Copilot summary diff, Zed)
  - Status: **Shipped 2026-09-02** — `ChangesPanel` aggregating all transcript diffs by path with per-file + bulk Accept/Revert, sharing the `diffResolved` reducer with the inline timeline. Diff bar in the sidebar nav.

### Medium-impact UX gaps

- [x] **No parallel/thread view for agents** (Cursor Agents Window, Copilot Agents Window, Zed threads, Claude Agent View)
  - We have: background task pills; no unified panel showing status/model/active-tool per session/thread.
  - Fix: a threads/sessions panel mapping to existing `EnginePool` workers + `/bg` tasks.
  - Status: **Shipped** — `ThreadsPanel.tsx` sidebar tab showing sessions and their status.
- [x] **No subagent status tracking row-by-row** (Copilot, Claude, Cursor track model + elapsed + active tool)
  - We have: `WorkerEvent::Subtask` but only a status label; no per-subagent model/tool/duration panel.
  - Status: **Shipped 2026-09-05** — backend `SubtaskStat` now carries `model`/`elapsed_ms`/`tool` (engine.rs, set by a `SubtaskReporter` in the orchestrator that attaches the subagent's pool model and re-emits "running" only on tool changes); frontend `agentRunStore` gained an `upsertSubtask`/`removeSubtask` `runningSubtasks` list (start time preserved across refresh events) and `ThreadsPanel` renders a live row per subagent with model chip + active `⎇ tool` + ticking elapsed (`formatMs(now - startedAt)` via its 1s ticker).
- [x] **No session resume picker / quick resume** (Claude `--resume`, Aider `/restore`, Zed thread switcher)
  - Status: **Shipped 2026-09-02** — `SessionResumePanel`: keyboard-driven (↑↓ / Enter) list of every recent chat across all projects, newest-first, with age + turn count. Reachable via the Resume sidebar tab and the palette.
- [x] **No model-hub downloader in the main model picker** (first-class "browse models" flow)
  - Status: **Shipped** — ModelBar has "Browse Models…" entry point (ModelBar.tsx:556-641) with HuggingFace hub search, download, and progress tracking surfaced directly in the model switcher dropdown.
- [x] **No context-window breakdown by category** (Claude Desktop, Copilot, Zed: system/MCP/messages/skills/memory split)
  - Status: **Shipped** — StatusBar `ContextBreakdownBar` (StatusBar.tsx:37-79) shows a segmented per-category bar (system, file, rules, skills, memory, pinned, turns) with per-category tooltips.
- [x] **Reasoning/tool tree not visualized** — no plan–subtask–tool graph view; step timeline exists but flat.
  - Status: **Shipped 2026-09-03** — `execGraphReducer` folds `plan-step`/`subtask`/`step`/`tool` events into a live parent→subtask→tool DAG (sibling `sequence` edges), rendered in `ExecutionGraphPanel` (bottom strip, Ctrl+Alt+G, View menu, palette).
- [x] **No /fork-as-background or "steer"/queued messages** — submit while agent busy queues; we have no mid-run interaction.
  - Status: **Shipped 2026-09-05** — mid-run interaction via a queue: `sendPrompt` when busy enqueues the message (`agentRunStore.enqueuePrompt`, a badged "N queued — will run…" hint in ChatPanel), and a flush effect drains one queued message whenever a turn ends, so steering prompts run serially instead of being dropped. ChatPanel's submit guard was relaxed accordingly. (**`/fork-as-background` since shipped 2026-09-05** — a `/fork <goal>` in `sendPrompt` starts an independent background task seeded with the current conversation while leaving the foreground transcript untouched; unlike queued messages it runs immediately.)

### Already balanced / areas we lead

- Plan mode, subagents, background tasks, voice input, audit log, YOLO mode, KV-cache reuse,
  context compaction + pressure indicator, multi-provider routing, MCP, checkpoints, todos,
  skills/rules, local-first, tool-call cards, edit-resubmit, export (PDF/DOCX/CSV).

See "Open-Source UI Frameworks (Enhance UX)" below for libraries that can accelerate
closing these gaps.

---

## Open-Source UI Frameworks (Enhance UX)

Recommended libraries/components to adopt, matched to gaps above. All are MIT/OSI
open source. The app already uses Tailwind v4 + `@monaco-editor/react`.

| Purpose | Library | Notes |
|---------|---------|-------|
| Agentic chat primitives (tool cards, streaming, attachments, action bar) | **@assistant-ui/react** | ~1.1M weekly downloads; composable primitives on Radix/shadcn; built-in streaming, generative UI, accessibility. Could replace/augment hand-rolled ChatPanel. |
| Streaming chat state + generative UI | **Vercel AI SDK** (`ai` / `@ai-sdk/react`) | `useChat` manages messages/status/error/regeneration; object + reasoning tokens; tool results → components. |
| Prebuilt AI components (Message, Reasoning, Tool, PromptInput, Attachment, Citation) | **Vercel AI Elements** | 20+ shadcn/ui-based components; install via CLI like shadcn. Fastest path to polished stream UI. |
| Command palette / quick-open | **cmdk** (or `@radix-ui`) | The de-facto command palette; used by Vercel/Linear. |
| Interactive terminal | **@xterm/xterm** (+ `xterm-addon-fit`) | PTY integration for an in-editor shell pane. |
| Accessible primitives (dialog/popover/tabs/select/tooltip) | **Radix UI** | Foundation for permission modals, model selector, tool cards. |
| Foundation components | **shadcn/ui** | Copy-paste components built on Radix+Tailwind; matches current Tailwind v4 stack. |
| State store (consolidate App.tsx) | **Zustand** | Already flagged in PROJECT_STATUS; pairs with the gaps above. |
| Graph/agent visualization (plan→subtask→tool) | ~~LangGraph UI~~ → **hand-rolled dead-tree DAG** (`execGraphReducer` + `ExecutionGraphPanel`) | Shipped 2026-09-03; no layout lib needed, per-node status + live accent. |

> Note: swapping to @assistant-ui / AI Elements is a **refactor** (large). For
> incremental wins, prefer **cmdk**, **@xterm/xterm**, OpenAI/Anthropic
> vision via existing attachment plumbing, and **Zustand** first; adopt the chat
> library only if a full ChatPanel rewrite is planned.

---

## Architecture & Design Gaps (Research 2026-08-29)

Research of how agentic AI tools architect their code (Claude Code layered
runtime/api/tools/commands + "Messages = State"; Cursor transport/session split +
agent-first IDE; Windsurf agent-first + auto-memories; Cline/Aider git-first,
file-based tooling) compared against our codebase. Current architecture flow:

```
ChatPanel.tsx:691 → App.tsx:1248 sendPrompt → api.agentRunTask/streamInference
  → src/lib/ipc.ts:44 tauriInvoke → src-tauri/src/main.rs command
  → EnginePool (LocalGenerator/RemoteGenerator) → orchestrator.rs:780 run_focused_steps
    → parse_tool_calls → tools.rs:193 giant match (no central registry)
    → results appended as "tool" messages → trim_working_history per step
  → agent:// events (ipc.ts:347) → useEngineEvents.ts → React state
```

### P0 — Architecture (Correctness / Scalability)

> **Shipped this session** — all three P0 architecture items below are now
> implemented: (1) ToolRegistry with unified schema/dispatch and a drift-guard
> test; (2) idempotent turns via client `turnId` + backend dedup; (3) runtime
> model hand-off via `RuntimeRouter` at orchestrator step time.

- [x] **No central runtime tool registry** (`src-tauri/src/agent/tools.rs:193`, `core.rs:82`)
  - Competitors: Claude Code / Cursor / Windsurf define tools as a registry of
    `{name, description, input_schema, handler, permissions}` entries; schemas
    and handlers derive from one source of truth.
  - We have: a **giant `match` statement** on `ToolCall` (`tools.rs:193-467`) and a
    **separate hand-maintained schema map** (`core.rs:82` `tool_schemas()`). The two
    can drift — a tool added to dispatch but not schema is invisible to the model;
    a schema tool with no dispatch arm errors at runtime.
  - Fix: Introduce a `ToolRegistry` (one `struct Tool { name, description, input_schema, dynamic, permission_class, handler }`), register all tools at startup, derive `tool_schemas()` and the subagent allow-lists from it so dispatch and schema can never diverge.
  - Status: **Fixed this session** — `ToolMeta` in `registry.rs` now carries `schema: fn() -> Value`; all tool schemas (incl. previously-missing `analyze_bug`/`review_code`/`browse_web`) live in the `TOOLS` table; `core::tool_schemas()` delegates to it; `validate()` + `ToolCall::all_tool_names()` guard `test_registry_matches_dispatcher`. Also fixed a pre-existing test-compile break in `subagent.rs` (`CHILD_NEVER` → `child_never()`).

- [x] **Request/response is not session-scoped as a message stream** (`src/lib/ipc.ts:44`, `main.rs:1199`)
  - Competitors: Claude Code treats **"Messages = State"** — every request returns the
    full updated message list, so the client is always in sync and idempotent
    (requests are additive-only, replay-safe).
  - We have: IPC is **command + event fire-and-forget**. The frontend maintains a
    parallel local `messages` React state mirroring the backend `ContextManager`;
    the two can desync (e.g. on a failed `tauriInvokeWrite` the JSONL write never
    retries — `ipc.ts:58-79`). There is no single source of truth for the turn.
  - Fix (incrementally): make write commands idempotent (client-generated turn UUID
    stored in the JSONL record; backend dedups on replay), and emit the authoritative
    message list (or a per-turn id) on turn end so the frontend reconciles against it.
  - Status: **Fixed this session** — every user/assistant/error record now carries a
    client `turnId` (`App.tsx` `newTurnId()`, shared by both halves of a turn via
    `sessionTurnRef`), and backend `session_append` no-ops a replay of an already-
    recorded turn id (`log_has_turn_id` + test). The chat transcript itself moved
    into the pure `chatReducer` store (see P1 below), giving "Messages = State".

- [x] **Provider routing is load-time only — no runtime model hand-off** (`main.rs:484-513`, `remote.rs:994`)
  - Competitors: Claude Code / Cursor / Windsurf hybridize — a **fast/cheap model
    handles planning/autocomplete, a flagship model handles edits**, and the app
    switches mid-task (MCP-supported role routing).
  - We have: `ProviderRegistry` with `Planner`/`Editor`/`Autocomplete`/`Embed` roles
    is consulted **only in `configure_remote_model`** to pick the generator that
    builds the whole pool; the orchestrator runs the **same single pool the entire
    turn**. `model_override` is only threaded into subagent prompts, never actual
    provider selection.
  - Fix: allow `route(role)` at orchestrator step time (keep a per-role handle
    cache), and let the plan step / subagents / compaction use a different role's
    model. This is the highest-leverage "feel like Cursor/Claude Code" win.
  - Status: **Fixed this session** — `ProviderRegistry` is `#[derive(Clone)]` and
    snapshotted per task; `engine::RuntimeRouter` lazily caches a `RemoteGenerator`
    per role and `resolve(ProviderRole)` returns a `RoutedGen` (pool handle for
    local/unmapped, remote otherwise). `run_agent_loop_pool` routes plan-mode,
    `plan_subtasks`, and `maybe_extract_memory` through `RuntimeRouter` with a
    pool-handle fallback; the flat execute loop keeps `primary` for KV-cache
    reuse. Test: `runtime_router_routes_local_roles_to_the_pool`.

### P1 — Architecture (Design Quality / Maintainability)

- [x] **Working history is a per-task in-memory `Vec` trimmed by real token counts** (`orchestrator.rs`)
  - Competitors: Claude Code uses exact token counting + a multi-tier cascade
    (budget → microcompact → context collapse → auto-compact), including LLM
    summarization; Cline/Roo track a real token budget.
  - We have: `trim_working_history` / `compress_large_messages` count via
    `tok_count`, which uses the **registered HF tokenizer** for exact
    `get_ids().len()` counts when one is available, else falls back to
    `est_tokens` (chars/4). **Stage-3 LLM summarization** ships
    (`summarize_old_block`, replaces the oldest unpinned block with a pinned
    summary; see the P0 compaction item). `maybe_extract_memory` writes
    post-hoc to disk but is not fed back mid-conversation.
  - The real tokenizer is threaded from the `ContextManager`: on local model
    load, `install_local_model` probes for a sibling `tokenizer.json` and
    registers it via `load_tokenizer`; `run_agent_loop_pool` (both foreground
    `stream_inference` and background `start_background_task` callers) clones
    it and passes it through `run_focused_steps`, `plan_subtasks`,
    `run_summary`, `run_subagents`/`drive_subagent`, and
    `execute_plan`/`execute_plan_inner` for exact-count compaction.
  - Remaining: when no sibling `tokenizer.json` ships with a model we still fall
    back to the chars/4 heuristic; a bundled/template tokenizer would remove that.

- [x] **No frontend state store — monolithic `App.tsx` (1809 lines, ~50 state slots)** (`src/App.tsx:71`)
  - Competitors: Cursor/Continue split UI state into a store + focused domain
    components and keep agent/transport state separate from rendering.
  - Status: **Partial — two slices shipped.** (1) The chat transcript moved into
    `src/lib/chatReducer.ts` (pure reducer, 14 unit tests). (2) **2026-09-02:**
    the custom-modes domain moved into a Zustand store `src/stores/customModes.ts`
    (load/reload/apply/system-prompt + modeAwareSystemPrompt + activeModeRef all
    owned by the slice; 5 unit tests), and `zustand` added as a dependency.
  - Remaining: sessions/model/policy state + the inline JSX callback extraction.
  - Status: **2026-09-05: three more reducer hosts shipped** — the chat transcript
    (`stores/transcriptStore.ts`, wraps `chatReducer`), the execution DAG
    (`stores/execGraphStore.ts`, wraps `execGraphReducer`) and the turn lifecycle
    (`stores/chatStatusStore.ts`, wraps `reduceChatStatus`) are now Zustand stores
    with stable `dispatch` fns and per-slice selectors; App.tsx reads `messages`,
    `ledger`, `graph`, `status` + dispatching from them (the 3 `useReducer` host
    slots dropped). The mid-run **queue moved into `agentRunStore`**
    (`queuedPrompts`/`queuedCount` + `enqueuePrompt`/`shiftPrompt`, tested). App.tsx
    remains a prop-feeding orchestrator; the remaining `useState` slices (genParams,
    verify/yolo/agentMode, savedRemote, workspace/chat/pane prefs) are intentionally
    left in place as low-risk UI mutations.

- [x] **Model-boundary contract is under-typed** (`src/types.ts`, `src/lib/ipc.ts:286`)
  - Competitors: facade layers (Claude Code's `api` struct) define typed request/
    response structs; the handler signature is the contract.
  - We have: backend commands return `serde_json::Value` in several places
    (`main.rs:1722,1741,1814`), settings are `Record<string, unknown>`, and four
    discriminated unions are typed as bare `string` (`src/types.ts:137,159,367,379`).
  - Fix: define typed Rust structs for audit/policy/settings and generate the
    TypeScript side; replace loose string unions with literal `|` unions (many items
    already listed under P2 — batch them with the registry work).
  - Status: **Shipped 2026-09-05 (final pass)** — Rust: `SessionRecord` struct with
    `#[serde(flatten)]` catch-all + camelCase so `session_append`/`session_load`
    round-trip client payloads without loss; `RemoteModelConfig` now `Serialize`
    and `AppSettings.remote` / `.last_chat` are typed (`LastChatPointer`);
    `SubtaskStat` carries `model`/`elapsed_ms`/`tool`. TS: `PolicyRule.policy` and
    `PolicySnapshot.default` literal unions, `WorkspaceChangedEvent.kind` union,
    `CheckpointInfo.name` + `NamedCheckpoint`, `SessionAppendRecord.role` union,
    SubtaskStat mirror. Remaining `Value` returns are tool-schema maps (dynamic by
    nature) and download-progress payloads.

### P2 — Architecture (Niche / Future)

- [x] **No agent-first editor integration beyond inline diffs** (`src/components/DiffView.tsx`, `EditorPane.tsx`)
  - Competitors: Cursor is agent-first — the editor is driven by the agent's
    proposals (apply/reject per hunk, hover previews), and transport/session are
    separate from the UI.
  - We have: agent edits surface as inline diff cards, but Monaco is not driven
    by an apply/reject-per-hunk protocol; model-boundary and UI-boundary are the
    same component.
  - Fix: define a `PureApplyRequest`/`ApplyResult` boundary between the diff engine
    and the editor, and let the editor consume hunks directly (per-hunk apply/reject).
  - Status: **Shipped 2026-09-04** — `HunkDecorations.tsx` parses unified diffs into hunks, renders Monaco glyph-margin decorations + hover actions on added lines, and recomputes file content per-hunk toggle via `applyPatchSelection` (pure unified patch utility in `unifiedPatch.ts`, 5 unit tests). Wired through `EditorPane` (pendingSections/hunkResolution/onToggleHunk props) and App (pendingSections memo, handleToggleHunk writes to disk via `writeTextFile`). CSS: `.hunk-glyph`, `.hunk-added-line`, `.hunk-reverted-line` in `index.css`.

- [x] **Subagent execution is same-process, same-model worker lease** (`orchestrator.rs:1542`, `subagent.rs`)
  - Competitors: Claude Code / Cursor can delegate to separate model/context
    processes; bounds and isolation come from the tool allow-lists and prompt.
  - We have: children run on the **same pool** via `WorkerLease` with depth guard +
    tool allow-lists — functionally solid, but no separate model/process Isolation.
  - Status: **Evaluated 2026-09-05 — accepted as a local-first limitation.**
    Multi-model subagent execution remains a documented future path (needs a
    per-role model handoff beyond the current run-time `RuntimeRouter`-by-role
    cache); per-subagent *model attribution* is already surfaced to the UI via the
    2026-09-05 SubtaskStat `model` field, so the missing piece is only true
    separate-model/process execution, not visibility or bounds. Revisit if
    multi-model subagents land.

---

## Already Shipped (Unique Advantages)

| Feature | Status |
|---------|--------|
| Local-first (no cloud required) | ✅ |
| 48+ agentic tools | ✅ |
| 4 subagent profiles (explore/implement/review/debug) | ✅ |
| Background tasks | ✅ |
| MCP server support | ✅ |
| Plan mode (Plan → Act separation) | ✅ |
| Voice input (whisper) | ✅ |
| Skills / rules system | ✅ |
| Audit log | ✅ |
| Red-zone safety (always-deny commands) | ✅ |
| YOLO mode (auto-approve routine) | ✅ |
| Session persistence | ✅ |
| Todo tracking (set/mark) | ✅ |
| Checkpoint / undo (git) | ✅ |
| File watcher (auto-reload) | ✅ |
| Export (PDF/DOCX/CSV) | ✅ |
| Edit-and-resubmit | ✅ |
| Destructive confirmation gate | ✅ |
| Error circuit breaker | ✅ |
| Markdown rendering in chat | ✅ |
| Escape key interrupt | ✅ |
| IPC retry with backoff | ✅ |
| Context pressure indicator | ✅ |
| Ctrl+Up/Down chat history | ✅ |
| Auto-checkpoint before edits | ✅ |
| Context trim warning | ✅ |
| Drag-and-drop file upload | ✅ |
| Token budget enforcement | ✅ |
| Git change summary tool | ✅ |
| .gitignore-aware file watching | ✅ |
| File explorer skeleton loading | ✅ |
| Multi-root workspace support | ✅ |
| Model hot-swap / selection UI | ✅ |
| Aider-style architect mode (two-model routing) | ✅ |
| Session fork / conversation branching | ✅ |
| Session search in ProjectsPanel | ✅ |
| KV-cache prefix reuse | ✅ |
| Tree-sitter grammars (JS/TS/Python/JSON/Rust) | ✅ |
| Monaco inline completion (autocomplete) | ✅ |
| Interactive terminal pane (xterm.js) | ✅ |
| Image / vision input in chat | ✅ |
| Changes panel (multi-file diffs) | ✅ |
| Threads/sessions panel | ✅ |
| Row-by-row subagent status (model + tool + elapsed) | ✅ |
| Queued mid-run messages (steer while busy) | ✅ |
| Dedicated Debug agent profile | ✅ |
| Session resume picker | ✅ |
| Command palette / quick-open | ✅ |
| Per-hunk Monaco integration | ✅ |
| Browser automation (browse_web) | ✅ |

---

## Comparison vs Competitors

| Feature | This Tool | Claude Code | Cursor | Aider | Windsurf |
|---------|-----------|-------------|--------|-------|----------|
| Local-first | **✅** | ❌ | ❌ | **✅** | ❌ |
| Subagents (4 profiles) | **✅** | ❌ | ❌ | ❌ | ❌ |
| Background tasks | **✅** | ❌ | ❌ | ❌ | ❌ |
| Voice input | **✅** | ❌ | ❌ | ❌ | ❌ |
| Audit log | **✅** | ❌ | ❌ | ❌ | ❌ |
| YOLO mode | **✅** | ❌ | ❌ | **✅** | ❌ |
| Todo tracking | **✅** | ❌ | ❌ | ❌ | ❌ |
| KV-cache reuse | **✅** | **✅** | **✅** | ❌ | **✅** |
| Inline editor diffs | **✅** | **✅** | **✅** | ❌ | **✅** |
| Markdown in chat | **✅** | **✅** | **✅** | **✅** | **✅** |
| Multi-root workspace | **✅** | ❌ | **✅** | ❌ | ❌ |
| Multi-model routing | **✅** | ❌ | **✅** | **✅** | ❌ |
| Conversation branching | **✅** | **✅** | ❌ | ❌ | ❌ |
| Per-tool timeout | **✅** | **✅** | **✅** | N/A | **✅** |
| Token budget | **✅** | ❌ | ❌ | ❌ | ❌ |
| Tree-sitter query | **✅** | ❌ | ❌ | ❌ | ❌ |

---

## Priority Roadmap

### Phase 1 — P0 Fixes (Bugs & Performance)
1. Fix `closeFile` setState-in-updater bug
2. Fix `syncAgentFile` event re-subscription (use filesRef)
3. Fix `parseEvent` crash (try/catch + validation)
4. Add `react-virtuoso` to message list
5. Memoize `buildTurnSegments` and `turnText`
6. Fix `openFilePicker` missing dep in keyboard shortcuts

### Phase 2 — P0 Agent Gaps (Critical Features) ✅ All Shipped
1. Persist skills active flags to `.ai/skills-state.json`
2. Persist session permissions + yolo to `.ai/session-permissions.json`
3. Add `/bug` command + `analyze_bug` tool
4. Add `/review` command + `review_code` tool
5. Implement auto-memory extraction to `.ai/memory.md`
6. Implement context compaction pipeline (multi-stage)

### Phase 3 — P1 UX & Performance (mostly shipped)
1. ✅ Implement model-based Monaco (preserve undo history)
2. ✅ Add click-outside + Escape for ModelBar dropdowns
3. ✅ Add DiffView virtualization for large diffs
4. ✅ Add request cancellation to remote model list fetch
5. ✅ Respect user scroll position in auto-scroll (2026-08-29)
6. ✅ Memoize `parseUnifiedDiff` and Monaco options
7. ✅ Extract inline JSX callbacks in App.tsx to `useCallback` (handleParamsChange/YoloChange/AttachClick/DetachFile/DropFiles)
8. ✅ `sessionAppend` retry dedup (idempotent via client `turnId` + backend `log_has_turn_id`)
9. ✅ Fix `Promise.all` listener leak

### Phase 4 — P1 Agent Gaps (Differentiation) ✅ All Shipped
1. Per-directory rules hierarchy (nested AGENTS.md)
2. Skill categories/tags + auto-suggest (suggest_skills tool, 2026-08-29)
3. Auto-verify after edits (lint/typecheck background)
4. Repo-map via symbol graph + PageRank (advertised to model, 2026-08-29)
5. Multi-provider model routing (registry wired into runtime, 2026-08-29)
6. Named checkpoints/snapshots

### Phase 5 — P2 Correctness & Polish
1. Add Escape to close modals
2. Add ARIA landmarks and `role="log"`
3. Debounce settings persistence
4. Fix DiffView reject error feedback
5. Fix `fileChangeNotice` setTimeout cleanup
6. Replace `std::fs` with `tokio::fs` in async commands
7. Type backend return values (replace `Value` with structs)
8. Type settings schema (`AppSettings`)
9. Fix chatStatus subtask label clobber
10. Add `agent://context-trimmed` handler
11. Clean up dead type fields and loose string unions

### Phase 6 — P2 Agent Gaps (Innovation) ✅ All Shipped
1. Custom agent modes (`.ai/modes/*.md`) — ✅ shipped
2. User-defined workflows (`.ai/workflows/*.md`) — ✅ shipped (2026-09-02)
3. Browser automation — ✅ shipped (`browse_web`, SSRF-guarded fetch; screenshot action returns clear error)
4. Session resume (`--continue`/`--resume`) — ✅ shipped (`session_load` replayed on workspace open)
5. LLM-based retrieval reranking — ✅ shipped 2026-09-04 (`rerank_results` wired into `semantic_search_codebase`)

### Phase 7 — P3 Polish
1. ✅ Add input length limit (2026-09-02)
2. ✅ Add unsaved-changes warning (2026-09-02)
3. ✅ Add Accept All / Reject All for multi-file diffs (2026-09-02)
4. ✅ Add loading indicators for workspace/session switching (spinner overlay, 2026-09-02)
5. ✅ Add unloadModel confirmation dialog (2026-09-02)
6. ✅ Add `aria-label` to icon buttons (2026-09-02)
7. ✅ Fix `useTokenStream` RAF cleanup (2026-09-02)
8. ✅ Add syntax highlighting to DiffView (2026-09-02)
9. ✅ Consolidate App.tsx state (chatReducer + customModes/modelStore/policyStore/filesStore/agentRunStore Zustand slices; **2026-09-05: transcript/execGraph/chatStatus reducer-stores + queue-in-store shipped**; App stays a prop-feeding orchestrator)

### Phase 8 — Competitive UI/UX (from 2026-08-31 gap analysis) ✅ Essentially Complete
1. ✅ Command palette / quick-open: Ctrl+P files, Ctrl+Shift+P actions/tools/sessions (hand-rolled, 2026-09-02)
2. ✅ Image/vision input in chat (paste → base64 → vision providers, 2026-09-04; `browse_web` screenshots render via image decode)
3. ✅ Integrated interactive terminal pane (`TerminalPanel.tsx` xterm.js + `terminal_spawn/write/kill/list`, Ctrl+Alt+T)
4. ✅ Monaco inline AI completion wired to the `Autocomplete`-role generator (`ensureInlineProvider` → `api.autocomplete` → `autocomplete_generate`)
5. ✅ Multi-file source-control / Changes panel (aggregate working-tree diffs with accept/revert, 2026-09-02)
6. ✅ Threads/sessions panel with per-session+status (`ThreadsPanel.tsx`); **row-by-row subagent rows added 2026-09-05** (per-subtask model chip + active tool + live elapsed)
7. ✅ Session resume picker (keyboard-driven, 2026-09-02)
8. ✅ Model-hub downloader surfaced in ModelBar ("browse models" flow, ModelBar.tsx:556-641)
9. ✅ Context-window breakdown by category (`ContextBreakdownBar`, StatusBar.tsx:37-79)
10. ✅ Plan–subtask–tool graph visualization (hand-rolled DAG from `plan-step`/`subtask`/`step`/`tool` events via `execGraphReducer`; `ExecutionGraphPanel` bottom strip, Ctrl+Alt+G, 2026-09-03)

**Framework adoption strategy** (incremental first, big-bang only when a ChatPanel rewrite is planned):
- **Adopt now**: `cmdk` (palette), `@xterm/xterm` (terminal ✅ adopted), OpenAI/Anthropic vision via existing attach plumbing (✅ adopted), `Zustand` (App.tsx consolidation).
- **Evaluate for a rewrite**: `@assistant-ui/react` or `Vercel AI Elements` to replace the hand-rolled ChatPanel; `LangGraph UI` patterns for agent graph visualization.

### Phase 9 — Agentic Modes (from 2026-08-31 modes gap analysis)
1. ✅ Wire `smalltalk` into `stream_inference` (main.rs) + orchestrator greeting short-circuit (`should_shortcut_smalltalk`, orchestrator.rs; skips model/tool round-trip for trivial greetings unless plan/decompose — 2026-09-03)
2. ✅ First-class **Ask (read-only)** built-in mode (`builtin_modes()`, subagent.rs) — surfaced via `agent_modes`, enforced on parent loop via `tool_allowed(mode, …)` (orchestrator.rs flat loop); **dedicated Debug profile added 2026-09-05** (`debug` in `PROFILES`: hypothesis → evidence → root-cause, max_steps 6, read-only diagnostics tool set, delegatable via `task`)
3. ✅ parent-loop custom-mode **`allowedTools`** enforcement (orchestrator.rs)
4. ✅ Implement **user-defined workflows** (`.ai/workflows/*.md` → `/workflow-name`, `allowedTools:` enforced, 2026-09-02)
5. ✅ Enforce custom-mode **`allowedGlobs`** (2026-09-02); **`modelOverride`** threaded into custom-mode children; **subagent model routing shipped 2026-09-05** (`SubGen`/`subgen_for_override` — a `task` child whose override names a registered remote provider runs on a dedicated `RemoteGenerator`, skipping the pool lease; unknown/local overrides fall back to the pooled worker)
6. ✅ Add **Code-Edit-Only** profile (propose edits, no terminal) — `edit` builtin mode (subagent.rs:306-351), no terminal tools, explicit "CODE-EDIT-ONLY mode" system prompt.
7. ✅ Implement true **dual-model Architect** via the `Editor` ProviderRole — flat execute loop routes `ProviderRole::Editor` through `RuntimeRouter` (orchestrator.rs:~775).
8. ✅ **Multi-model subagent execution (2026-09-05)** — a `task` subagent's `modelOverride` that names a registered (non-Local) provider now routes the child to a dedicated `RemoteGenerator` via `SubGen` (`ProviderRegistry::find_best_remote_provider`, remote.rs; `subgen_for_override`, orchestrator.rs) and skips the pool-worker lease entirely; unknown/local/misconfigured overrides gracefully fall back to the pooled worker. Registry is threaded as a reborrowed reference through `run_agent_loop_pool` → `run_focused_steps` → `run_subagents` → `drive_subagent` (and `execute_plan`/`execute_plan_inner`). Tests: `find_best_remote_provider_matches_model_id_or_name_case_insensitive`; 160 Rust tests green.
9. ✅ **Bundled tokenizer fallback (2026-09-05)** — Qwen2.5-Coder BPE `tokenizer.json` (`Qwen/Qwen2.5-Coder-7B-Instruct`, **Apache-2.0**) embedded via `include_bytes!` at `src-tauri/assets/tokenizer.json` and lazily parsed once (`BUNDLED_TOKENIZER` `OnceLock`, context.rs); `ContextManager::new` now defaults to exact BPE token counts instead of the chars/4 heuristic, which remains only as a last-resort fallback (and for `est_tokens` in orchestrator when no tokenizer is threaded). A user-supplied `tokenizer.json` still overrides via `load_tokenizer` (main.rs:406). Test: `bundled_tokenizer_loads_and_counts_tokens_exactly` (and `pinned_survive_eviction` tightened for exact-count semantics).
10. ✅ **Token-budget-aware tool fan-out cap (2026-09-05)** — single-step parallel dispatch is capped at `MAX_PARALLEL_FANOUT = 6` (`fanout_batch_size`, orchestrator.rs); when the working history is already over the 80% eviction budget the batch drops to fully sequential so a reply hammering out dozens of tool calls can't blow the context in one step. Test: `fanout_caps_parallel_batches_and_falls_back_sequential_over_budget`.
11. ✅ **`/fork-as-background` (2026-09-05)** — new `/fork <goal>` slash command (App.sendPrompt, App.tsx) spins up an independent background task seeded with the **current** conversation (the backend's `start_background_task` snapshots the whole `ContextManager`, so the fork carries full context + rules/skills), while leaving the foreground transcript untouched — no user turn is pushed. The fork prompt runs with planMode off, verify off, up to 12 steps; a background pill + a one-line marker note appear in chat; usable while streaming (meant to be), unlike ordinary deferred messages (`/bg` untouched). Slash hints added for `/bg` and `/fork` (ChatPanel.tsx).

