# AI Code Editor — Project Status

**Last Updated**: 2026-08-29 (architecture/design gap analysis added — see "Architecture & Design Gaps" below)
**Stack**: Tauri 2 (Rust) + React 19 + Vite 6 + Tailwind v4 + Monaco editor
**Inference**: llama.cpp GGUF (local) + OpenAI-compatible SSE (remote), multi-provider routing (Planner/Editor/Autocomplete/Embed roles)
**Agent**: ReAct loop with 48+ tools, 3 subagent profiles, plan mode, decompose mode, background tasks, custom modes, auto-memory extraction, multi-stage context compaction, repo-map (PageRank), skill auto-suggest

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

- [ ] **No message list virtualization** (`src/components/ChatPanel.tsx:874`)
  - Status: Deferred — requires `react-virtuoso` dependency install; will address in next session.

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
  - Status: Fixed — `compact_context()` multi-stage pipeline exists and is now **wired into the live agent loop**: `trim_working_history` (orchestrator.rs:2280) first compresses oversized messages (tool outputs / long replies / non-system pinned buffers) around a head+tail marker (`compress_large_messages`), then evicts oldest non-pinned messages. LLM conversation-summarization (stage 3) intentionally deferred — the two deterministic stages keep payloads under budget without a generation handle. Tested.

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
- [ ] **12+ inline JSX callbacks in App.tsx defeat child memoization** (`src/App.tsx:1435-1674`)
  - `onParamsChange`, `onYoloChange`, `onAttachClick`, `onDetachFile`, `onDropFiles`, `skills` array — all unstable
  - Fix: Extract to `useCallback`, pass stable refs
- [ ] **`sessionAppend` retry can duplicate JSONL records** (`src/lib/ipc.ts:289`)
  - Status: Mitigated — the write path (`tauriInvokeWrite`) never retries writes (ipc.ts:58-79), so a transient failure surfaces loudly instead of duplicating a JSONL line. Idempotency keys are not present.
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

- [ ] **Escape only works during streaming — no Escape for modals** (`src/App.tsx:1037`)
  - Users expect Escape to close Settings, Knowledge, Permission dialogs
  - Fix: Add Escape handler that closes open modal

- [ ] **No ARIA landmarks or roles** (`src/App.tsx:1415`, `src/components/ChatPanel.tsx:861`)
  - No `<main>`, `<nav>`, `<aside>`, `role="log"`, `aria-live="polite"` on message list
  - Fix: Add semantic landmarks and ARIA attributes

- [ ] **Debounce settings persistence** (`src/App.tsx:696-717`)
  - Read-modify-write cycle fires on every workspace/chat/params change; race conditions on rapid updates
  - Fix: Debounce 500ms, or write directly to a ref with flush-on-unmount

- [ ] **DiffView reject silently swallows errors** (`src/components/DiffView.tsx:58`)
  - `revertFile` failure gives no feedback; user thinks revert worked
  - Fix: Show error toast or inline error message on failure

- [ ] **`fileChangeNotice` setTimeout not cleaned up on unmount** (`src/App.tsx:554`)
  - Multiple timeouts created on rapid changes; never cleared on unmount
  - Fix: Store timeout ID in ref, clear in useEffect cleanup

- [ ] **Synchronous `std::fs` in async Tauri commands** (`src-tauri/src/main.rs:655,1229,2124`)
  - `persist_model_path`, `list_directory`, `session_append` block Tokio runtime
  - Fix: Replace with `tokio::fs` or `spawn_blocking`

- [ ] **Backend commands return `serde_json::Value` instead of typed structs** (`src-tauri/src/main.rs:1722,1741,1814`)
  - Audit, policy, checkpoints return untyped JSON; structural drift invisible to compiler
  - Fix: Define typed Rust structs, derive Serialize, use in command return types

- [ ] **Settings schema completely untyped** (`src/lib/ipc.ts:286`)
  - `settingsLoad` returns `Record<string, unknown>`; every consumer defensively casts
  - Fix: Define `AppSettings` struct in Rust, generate TypeScript type

- [ ] **`chatStatus` subtask done clears label unconditionally** (`src/lib/chatStatus.ts:181`)
  - If a tool started between subtask "running" and "done" events, the tool's label is clobbered
  - Fix: Only clear label if it belongs to the finishing subtask

- [ ] **`agent://context-trimmed` event emitted but not subscribed** (`src/lib/events.ts`)
  - Backend emits when history trimmed >50%; frontend ignores it
  - Fix: Add handler, show notification in UI

- [ ] **Dead `AgentToolEvent.output` field** (`src/types.ts:64`)
  - Field exists in TypeScript but is never emitted by backend
  - Fix: Remove field, or wire it up in backend

- [ ] **Four discriminated unions typed as bare `string`** (`src/types.ts:137,159,367,379`)
  - `InferenceDone.outcome`, `AuditEntry.decision`, `BackgroundTaskInfo.status`, `BackgroundTaskEvent.status`
  - Fix: Replace with `"a" | "b" | "c"` union types

- [ ] **`AgentToolEvent.sessionId` optional in TS but always present in Rust** (`src/types.ts:66`)
  - Unnecessary null-checks downstream
  - Fix: Remove `?` from type definition

### P2 — Agent Gaps (New)

- [ ] **No custom agent modes** (`src-tauri/src/agent/subagent.rs`)
  - Competitors: Roo Code `.roomodes` (YAML-defined modes with tool restrictions), Claude Code custom agents in `.claude/agents/`
  - We have: Fixed 3 subagent profiles (explore/implement/review)
  - Fix: Load custom mode definitions from `.ai/modes/*.md` — each mode specifies name, description, system prompt, allowed tools, file glob restrictions, model override. UI mode switcher.

- [ ] **No user-defined workflows/recipes** (`src/components/ChatPanel.tsx`)
  - Competitors: Windsurf `.windsurf/workflows/` (reusable agentic recipes), Continue.dev configurable slash commands
  - We have: Hardcoded slash commands (/plan, /bg, /clear, etc.)
  - Fix: Load workflow definitions from `.ai/workflows/*.md` — each workflow is a markdown file with trigger command, system prompt template, and tool restrictions. Invoked via `/workflow-name`.

- [ ] **No browser automation** (`src-tauri/src/agent/tools.rs`)
  - Competitors: Cline Puppeteer (navigate, click, type, screenshot), Cursor Browser tool
  - We have: No browser integration
  - Fix: Add `browse_web` tool using headless Chromium — navigate, click, type, screenshot, extract text. Enable visual bug fixing and E2E testing workflows.

- [ ] **No session resume** (`src-tauri/src/main.rs`)
  - Competitors: Claude Code `--continue`/`--resume`, Aider `/restore-session`/`--restore-chat-history`
  - We have: Sessions stored as JSONL but no resume capability
  - Fix: Add session resume — load previous session's messages, model selection, agent state. `--continue` for most recent, `--resume` for session picker.

- [ ] **No LLM-based retrieval reranking** (`src-tauri/src/agent/tools.rs`)
  - Competitors: Windsurf M-Query, Cursor reranking layer
  - We have: TF-IDF cosine similarity (no reranking)
  - Fix: After initial TF-IDF retrieval, use LLM to rerank top-K results by relevance to query. Reduces noise in semantic search results.

---

## P3 — Polish

- [ ] **No input length limit on chat textarea** (`src/components/ChatPanel.tsx:1018`)
  - User can paste multi-MB strings; `/` command parsing doesn't guard against oversized input
  - Fix: Add `maxLength` attribute and validation in `submit()`

- [ ] **No unsaved-changes warning on file close** (`src/App.tsx:958`)
  - `closeFile` doesn't check `f.saved` before closing
  - Fix: Check `saved` flag, show confirmation dialog if dirty

- [ ] **No "Accept All" / "Reject All" for multi-file diffs** (`src/components/ChatPanel.tsx:528`)
  - Turns with many file changes require individual accept/reject per file
  - Fix: Add bulk action buttons when >1 diff present

- [ ] **No loading indicators for workspace/session switching** (`src/App.tsx:1073,1052`)
  - `applyWorkspace` and `loadSessionIntoView` have no loading state
  - Fix: Add spinner or skeleton during async operations

- [ ] **`unloadModel` destroys chat view without confirmation** (`src/App.tsx:757`)
  - Destructive action wipes visible conversation with no undo
  - Fix: Add confirmation dialog before unloading

- [ ] **No `aria-label` on icon buttons** (Multiple components)
  - "+", "✦", "📁" buttons have no accessible names; screen readers announce "button"
  - Fix: Add `aria-label` to all icon-only buttons

- [ ] **`useTokenStream` RAF not cancelled on unmount** (`src/hooks/useTokenStream.ts:30`)
  - `requestAnimationFrame` callback runs against unmounted component state
  - Fix: Store RAF ID, cancel in useEffect cleanup

- [ ] **No syntax highlighting in DiffView** (`src/components/DiffView.tsx`)
  - Diffs render as plain text; no code highlighting for changed lines
  - Fix: Apply language-specific syntax highlighting to diff content

- [ ] **Consolidate App.tsx state** (`src/App.tsx:70-148`)
  - 40+ useState calls in one component; massive candidate for splitting
  - Fix: Extract sub-state into useReducer or split into sub-components with own state

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

- [ ] **No central runtime tool registry** (`src-tauri/src/agent/tools.rs:193`, `core.rs:82`)
  - Competitors: Claude Code / Cursor / Windsurf define tools as a registry of
    `{name, description, input_schema, handler, permissions}` entries; schemas
    and handlers derive from one source of truth.
  - We have: a **giant `match` statement** on `ToolCall` (`tools.rs:193-467`) and a
    **separate hand-maintained schema map** (`core.rs:82` `tool_schemas()`). The two
    can drift — a tool added to dispatch but not schema is invisible to the model;
    a schema tool with no dispatch arm errors at runtime.
  - Fix: Introduce a `ToolRegistry` (one `struct Tool { name, description, input_schema, dynamic, permission_class, handler }`), register all tools at startup, derive `tool_schemas()` and the subagent allow-lists from it so dispatch and schema can never diverge.

- [ ] **Request/response is not session-scoped as a message stream** (`src/lib/ipc.ts:44`, `main.rs:1199`)
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

- [ ] **Provider routing is load-time only — no runtime model hand-off** (`main.rs:484-513`, `remote.rs:994`)
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

### P1 — Architecture (Design Quality / Maintainability)

- [ ] **Working history is a per-task in-memory `Vec` trimmed by char heuristics** (`orchestrator.rs:2280`)
  - Competitors: Claude Code uses exact token counting + a multi-tier cascade
    (budget → microcompact → context collapse → auto-compact), including LLM
    summarization; Cline/Roo track a real token budget.
  - We have: `trim_working_history` / `compress_large_messages` use `est_tokens`
    (chars/4 heuristic) and **drop/truncate only** — no mid-task LLM summarization;
    `maybe_extract_memory` writes post-hoc to disk but is not fed back mid-conversation.
  - Fix: wire the real tokenizer (already available in `ContextManager`) into the
    orchestrator's budget, and add an optional **stage-3 LLM summarization** (using a
    cheap-role model) that replaces the oldest block with a summary instead of
    discarding it.

- [ ] **No frontend state store — monolithic `App.tsx` (1809 lines, ~50 state slots)** (`src/App.tsx:71`)
  - Competitors: Cursor/Continue split UI state into a store + focused domain
    components and keep agent/transport state separate from rendering.
  - We have: all state in one component, mirrored into refs; child components are
    presentational and re-render on App re-render; `12+ inline JSX callbacks`
    (`src/App.tsx:1435-1674`) defeat memoization (already listed under P1).
  - Fix: extract sessions, agent-run, model, and policy into either a reducer or a
    small external store (Zustand) so event handlers and components subscribe to
    slices instead of the whole tree.

- [ ] **Model-boundary contract is under-typed** (`src/types.ts`, `src/lib/ipc.ts:286`)
  - Competitors: facade layers (Claude Code's `api` struct) define typed request/
    response structs; the handler signature is the contract.
  - We have: backend commands return `serde_json::Value` in several places
    (`main.rs:1722,1741,1814`), settings are `Record<string, unknown>`, and four
    discriminated unions are typed as bare `string` (`src/types.ts:137,159,367,379`).
  - Fix: define typed Rust structs for audit/policy/settings and generate the
    TypeScript side; replace loose string unions with literal `|` unions (many items
    already listed under P2 — batch them with the registry work).

### P2 — Architecture (Niche / Future)

- [ ] **No agent-first editor integration beyond inline diffs** (`src/components/DiffView.tsx`, `EditorPane.tsx`)
  - Competitors: Cursor is agent-first — the editor is driven by the agent's
    proposals (apply/reject per hunk, hover previews), and transport/session are
    separate from the UI.
  - We have: agent edits surface as inline diff cards, but Monaco is not driven
    by an apply/reject-per-hunk protocol; model-boundary and UI-boundary are the
    same component.
  - Fix: define a `PureApplyRequest`/`ApplyResult` boundary between the diff engine
    and the editor, and let the editor consume hunks directly (per-hunk apply/reject).

- [ ] **Subagent execution is same-process, same-model worker lease** (`orchestrator.rs:1542`, `subagent.rs`)
  - Competitors: Claude Code / Cursor can delegate to separate model/context
    processes; bounds and isolation come from the tool allow-lists and prompt.
  - We have: children run on the **same pool** via `WorkerLease` with depth guard +
    tool allow-lists — functionally solid, but no separate model/process Isolation
    (acceptable for local-first; revisit if multi-model subagents land).

---

## Already Shipped (Unique Advantages)

| Feature | Status |
|---------|--------|
| Local-first (no cloud required) | ✅ |
| 47+ agentic tools | ✅ |
| 3 subagent profiles (explore/implement/review) | ✅ |
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

---

## Comparison vs Competitors

| Feature | This Tool | Claude Code | Cursor | Aider | Windsurf |
|---------|-----------|-------------|--------|-------|----------|
| Local-first | **✅** | ❌ | ❌ | **✅** | ❌ |
| Subagents (3 profiles) | **✅** | ❌ | ❌ | ❌ | ❌ |
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
7. ⏳ Extract inline JSX callbacks in App.tsx to `useCallback`
8. ⏳ `sessionAppend` retry dedup (mitigated — write path never retries)
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

### Phase 6 — P2 Agent Gaps (Innovation)
1. Custom agent modes (`.ai/modes/*.md`) — ✅ shipped
2. User-defined workflows (`.ai/workflows/*.md`)
3. Browser automation (Puppeteer)
4. Session resume (`--continue`/`--resume`) — ✅ shipped (`session_load` replayed on workspace open)
5. LLM-based retrieval reranking

### Phase 7 — P3 Polish
1. Add input length limit
2. Add unsaved-changes warning
3. Add Accept All / Reject All for multi-file diffs
4. Add loading indicators for workspace/session switching
5. Add unloadModel confirmation dialog
6. Add `aria-label` to icon buttons
7. Fix `useTokenStream` RAF cleanup
8. Add syntax highlighting to DiffView
9. Consolidate App.tsx state
