# Project Status — AI Editor

_Last updated: 2026-08-20 (P0 backlog shipped + P1-6 persistent plan tools shipped:
turn lifecycle & outcomes, token accounting, remote stall/retry, audit log,
permission decision memory, create_plan/read_plan/update_plan/execute_plan).
This file is the source of truth for the session's progress. Read it at session
start; update it whenever milestones change. Strategic plan: see `ROADMAP.md`._

---

## BLUEPRINT GAP ANALYSIS — vs the agentic-coding-tool blueprint

Reviewed `D:\software\cursor\prompt.txt` (engineering blueprint distilled from a
production agentic coding tool). **Excluded as custom Cursor tools/APIs:**
ConnectRPC/protobuf transport (we use Tauri commands + events), extension-host
hooks (`PreToolUse`/`PostToolUse`/`execute_hook`/`AfterAgentThought`/…), Cursor
subagent types (`CURSOR_GUIDE`, `MEDIA_REVIEW`, `BROWSER_USE`, `COMPUTER_USE`),
computer-use, and the cloud/remote worker infra (VM targets, agent stores,
"babysit PR"). Everything else below is a genuine gap we can build on our stack.

### Already implemented (inventory, matches blueprint sections)

| Blueprint § | What we have |
|---|---|
| §1 architecture | Single-process split UI ⇄ Rust (Tauri IPC + events); orchestrator + tool layer + engine pool in the backend process |
| §2 transport | Tauri `invoke` commands + `emit` events (not protobuf); local GGUF via llama.cpp + remote OpenAI-compatible SSE |
| §3 turn lifecycle | `inference-started` / `inference-done` / `inference-error` / `execution-aborted` + session ids; `InferenceDone.outcome` = `completed \| failed \| interrupted \| error` with UI badges |
| §4 streaming | `token_delta` (rAF-batched), `agent-step`, `agent-tool-event`, `agent-subtask`, `agent://tool-output` (shell streaming), `agent://plan-step` (plan item progress), diffs, summary text; `turn_ended` token breakdown (input/output/cache-read/cache-write/reasoning) |
| §5 tool loop | generate → parse `<execute_tool>` → policy check → dispatch → feedback; per-call started/done/error tool cards; circuit breaker; self-healing retry; `TruncatedToolCall` equivalent via caps |
| §6 permissions | red-zone deny (unconditional), `default_allow` read-only list, per-tool allow/ask/deny rules in `.ai/policy.json`, `ask` → `agent://permission-request` → UI **allow-once / allow-session / always-allow / deny** (decision memory); workspace path scoping |
| §7 tool catalog | 21 tools: glob/search/semantic-search/view-structure/read/apply-diff/write/terminal(streaming)/MCP/run-tests/git(status,diff,commit,checkpoint,revert)/create-skill/create_plan/read_plan/update_plan/execute_plan |
| §8 subagents | Sub-task decomposition (`/decompose`) now **parallel** via engine pool (one worker per subtask), per-subtask step-group labels |
| §10 sessions | JSONL session log (`session_append`/`session_load`) replayed into chat + model context on workspace open |
| §11 planning | Plan mode: `/plan` → plan text → **Approve & Execute** / Reject; persistent `create_plan`/`execute_plan` with `.ai/plan.json` + `.ai/plan.md`; git checkpoints + one-click revert + auto-checkpoint before plan execution |
| §13 context | System-prompt assembly (rules + skills + MCP + active-file buffer), tokenizer-accurate counting, 80% sliding-window eviction |
| §15 tests | 24 backend unit tests + ignored live-GGUF headless chat test |

### Missing concepts → backlog (prioritized)

**P0 — trust & turn quality (highest ROI) — ✅ shipped (2026-08-15)**
1. **Turn lifecycle & outcomes** — `inference-done` now carries `outcome`
   (`completed | failed | interrupted | error`, derived from stop reason /
   stuck / cancelled) and the UI shows a per-turn badge + footer label.
2. **Token accounting breakdown** — `InferenceDone` carries
   `input_tokens / output_tokens / cache_read / cache_write / reasoning_tokens`
   (blueprint §4 `turn_ended`); local = honest (write=input, read=0 — KV is
   cleared per run), remote = provider `usage` parsed when present (OpenAI
   `prompt_tokens`/`cached_tokens`/`reasoning_tokens`, Anthropic
   `message_start`/`message_delta`).
3. **Remote stall detection + retries** — 90s stall watchdog aborts a silent
   stream with a typed error; transient request failures (connect error,
   408/429/5xx) retry with exponential backoff (max 2) before streaming begins.
4. **Audit log** — every tool-call policy verdict is appended to
   `.ai/audit.jsonl` (tool, summary, decision, latency, success, error);
   `agent_audit_log` command + **AuditMenu** panel in the status bar.
5. **Permission decision memory** — `allow_once / allow_session / always_allow /
   deny`; session memory is in-process (`ToolState.session_allow`, exact
   command for terminal), `always_allow` writes a rule to `.ai/policy.json`.

**P1 — agent capabilities**
6. **`create_plan` / `execute_plan`** — persistent markdown plan file the agent can edit; `step_started/step_completed` per plan item (§11). ✅ shipped (2026-08-20): `create_plan`, `read_plan`, `update_plan`, `execute_plan` tools; `.ai/plan.json` + `.ai/plan.md` persistence; `agent://plan-step` events in timeline; orchestrator intercepts `execute_plan` for per-item focused loops.
7. **Goals & todos** — `create_goal/update_goal` (`OPEN|IN_PROGRESS|COMPLETED|TERMINAL`), `read_todos/update_todos` derived from the plan (§11).
8. **First-class subagents** — `task` tool with `subagent_type` (`EXPLORE|BASH|DEBUG|CUSTOM`), per-child restricted `permission_mode`, `subagent_await` (§8). Today we have parallel decompose, not spawn/await.
9. **`ask_question` / `send_to_user`** — async human interaction query mid-task (§7 Human tools; §3 `awaiting_input`).
10. **Git toolchain** — add `blame`, `push`, `pull`, `create_branch`, `create_pr`, `pr_status`, `ci_status` (§7 Git/CI/PR).
11. **File tool gaps** — `delete`; `read_lints`/`diagnostics` (tree-sitter backed, §7 Files).

**P2 — concurrency & UX**
12. **Background work & multitasking** — `spawn_background_shell` / `background_subagent` that survive turn end, pill/badge UI (not a modal), `abort_background_work` (§9).
13. **Session management UI** — `list_sessions`, `fork_session`, watch lifecycle, statuses `AWAITING_INPUT|ERROR|ABORTED` (§10).
14. **Modes** — `ASK` (every tool prompts), `DEBUG`, `CUSTOM` (per-mode system prompt + tool allowlist), `switch_mode` (§12). Today: AGENT vs PLAN only.

**P3 — scale & polish**
15. **Compaction** — at ~80% context summarize older messages into a `ConversationSummaryArchive` instead of hard-evicting (§11; PreCompact equivalent).
16. **Context usage tree + blob store** — per-component token contribution + blob store for large context (§13).
17. **Smart-mode classifier tier** — lightweight local risk classifier + natural-language `allow_instructions/block_instructions` between allowlists and manual review (§6 tier 2).

### Next implementation target
P1 items 7–11 (goals & todos, first-class subagents, `ask_question`/`send_to_user`,
git toolchain extensions, file-delete/lints). P1-6 (persistent plan tools)
shipped 2026-08-20. See the **TODO / implementation log** table below.

---

## TODO / implementation log

| # | Item | Status |
|---|---|---|
| — | Blueprint gap analysis (pass) | ✅ done |
| P0-1 | Turn lifecycle & outcomes + UI badges | ✅ done |
| P0-2 | Token accounting breakdown (`inference-done`) | ✅ done |
| P0-3 | Remote stall detection + retry/backoff | ✅ done |
| P0-4 | Tool-verdict audit log `.ai/audit.jsonl` + UI | ✅ done |
| P0-5 | Permission decision memory (allow_once / always_allow / ask_first) | ✅ done |
| P1-6 | `create_plan` / `execute_plan` (markdown plan file + per-item steps) | ✅ done |
| P1-7 | Goals & todos tools | pending |
| P1-8 | First-class subagents (`task` + restricted child perms + `subagent_await`) | pending |
| P1-9 | `ask_question` / `send_to_user` | pending |
| P1-10 | Git: blame / push / pull / branches / PR / CI | pending |
| P1-11 | File: delete / read_lints / diagnostics | pending |
| P2-12 | Background work + multitasking (pill UI, abort) | pending |
| P2-13 | Session management UI (list / fork / watch) | pending |
| P2-14 | Modes (ASK / DEBUG / CUSTOM / switch_mode) | pending |
| P3-15 | Context compaction (summarize @ 80%) | pending |
| P3-16 | Context usage tree + blob store | pending |
| P3-17 | Smart-mode classifier tier + NL allow/block rules | pending |

---

## ✅ SHIPPED — P0 trust & turn quality (turn lifecycle, token accounting, remote stall/retry, audit log, permission memory) + P1-6 persistent plan tools, builds green

- `cargo check` clean (zero warnings), `cargo test` **24/24** (1 ignored
  live-GGUF), `npm run build` green.

### What changed this pass (2026-08-20, P1-6 plan tools)

**1. `create_plan` / `read_plan` / `update_plan` tool dispatch (P1-6)**
- `plan.rs`: `PlanStatus` gains `#[derive(Default)]` (default = `NotStarted`) so
  `#[serde(default)]` on `PlanItem.status` compiles.
- `tools.rs`: new `create_plan`, `read_plan`, `update_plan` async fn
  implementations wired into the `dispatch` match. `create_plan` builds a
  `PlanState` via `plan::new_plan`, saves both `.ai/plan.json` and `.ai/plan.md`,
  and caches it in `ToolState.plan`. `read_plan` loads from cache or disk and
  returns the rendered markdown with completion stats. `update_plan` mutates a
  single item's status/details, persists both files, and refreshes the cache.
  `ExecutePlan` is intercepted by the orchestrator before dispatch (existing
  path) — a defensive error arm is present but unreachable.
- `orchestrator.rs`: `set_plan_status` param renamed to `_plan_id` (was unused).

**2. Frontend wiring for plan step events**
- `types.ts`: new `PlanStepEvent` interface (`sessionId`, `planId`, `itemIndex`,
  `title`, `status`, `error?`).
- `events.ts`: `PlanStepHandlerEvent` type alias + `onPlanStep?` in
  `EngineHandlers`.
- `ipc.ts`: `agent://plan-step` event subscription wired in
  `subscribeEngineEvents`.
- `hooks/useEngineEvents.ts`: `onPlanStep` passthrough.
- `App.tsx`: `onPlanStep` handler appends a step chip (`Plan · <title>`) to the
  current message's timeline on `in_progress`.

**3. System prompt updated**
- `prompt.ts`: tools 13–16 document `create_plan`, `read_plan`, `update_plan`,
  `execute_plan` with JSON examples so the model knows when to use them.

### Changed files (this pass)
- `src-tauri/src/agent/plan.rs` — `PlanStatus` derives `Default`
- `src-tauri/src/agent/tools.rs` — `create_plan`/`read_plan`/`update_plan`
  dispatch arms + implementations; `plan` module import
- `src-tauri/src/agent/orchestrator.rs` — `_plan_id` rename
- `src/types.ts` — `PlanStepEvent`
- `src/lib/events.ts` — `PlanStepHandlerEvent`, `onPlanStep?`
- `src/lib/ipc.ts` — `onPlanStepEvent` subscription
- `src/hooks/useEngineEvents.ts` — `onPlanStep` passthrough
- `src/App.tsx` — `onPlanStep` handler, `PlanStepEvent` import
- `src/lib/prompt.ts` — plan tools 13–16 in system prompt
- `PROJECT_STATUS.md` / `ROADMAP.md` — kept in sync

### Next step
Smoke test `npm run tauri:dev`: (a) agent mode → model calls `create_plan` →
confirm `.ai/plan.json` + `.ai/plan.md` created; (b) model calls `update_plan`
→ confirm status persisted; (c) model calls `execute_plan` → watch per-item
focused loops run with plan-step chips in the timeline. Then P1-7: goals &
todos tools.

---

### What changed this pass (2026-08-15, P0 backlog)

**1. Turn lifecycle & outcomes (P0-1)**
- `InferenceDone` gains `outcome` (`"completed" | "failed" | "interrupted" |
  "error"`): local/remote map `stop_reason == "cancelled"` → `interrupted`;
  orchestrator maps `stuck` → `failed`, `cancelled` → `interrupted`, else
  `completed` (typed `WorkerEvent::Error` stays the ERROR path).
- UI: per-turn outcome badge on assistant messages (`OutcomeBadge`) and a
  footer label (`done/failed/interrupted/error`) in `ChatPanel`.

**2. Token accounting breakdown (P0-2)**
- `InferenceDone` carries `input_tokens / output_tokens / cache_read_tokens /
  cache_write_tokens / reasoning_tokens`.
- Local (`run_generation`): honest accounting — KV cache is cleared per run, so
  `cache_write = prompt_len`, `cache_read = 0`, `input = prompt_len`,
  `output = total_tokens`, `reasoning = 0`.
- Remote (`remote.rs`): `RemoteUsage` parses provider usage — OpenAI
  `prompt_tokens` + `prompt_tokens_details.cached_tokens` +
  `completion_tokens_details.reasoning_tokens`; Anthropic `message_start`
  input/cache + `message_delta` output; chars/4 fallback when omitted.
- Orchestrator aggregates the breakdown across steps/subtasks/summary so a whole
  task reports correct totals (`FocusOutcome`/`SubResult` carry the fields).

**3. Remote stall detection + retry/backoff (P0-3)**
- 90s stall watchdog: a fresh `tokio::time::sleep` per chunk in the stream
  `select!` aborts with a typed error when the provider goes silent.
- `send_with_retry`: retries connect errors and 408/429/5xx with exponential
  backoff (1s/2s, max 2) — only *before* streaming begins, never mid-stream
  (which would duplicate output).

**4. Audit log `.ai/audit.jsonl` (P0-4)**
- `tools.rs::audit` appends one JSONL record per tool call at each dispatch exit:
  `{ts, id, tool, summary, decision, startedAt, latencyMs, success, error}`.
  Decisions: `allow | deny | granted | granted-session | granted-always |
  declined | timed-out | aborted`. Summary only — raw args/file contents are
  never logged (no secrets in the audit trail).
- `agent_audit_log(limit)` command (newest first) + **AuditMenu** status-bar
  panel (decision badges, recency, ok/blocked summary).

**5. Permission decision memory (P0-5)**
- `PermissionDecision` enum (`allow_once | allow_session | always_allow |
  deny`) rides the permission-response channel; `agent_respond_permission`
  now takes `decision: String`.
- Session memory: `ToolState.session_allow` (`HashSet`), keyed by tool name —
  or the exact command for `execute_terminal_command` (one approved `cargo
  test` never silently unlocks a different command). `policy::check` consults
  it before asking.
- `always_allow`: `policy::remember_always` merges an `allow` rule into
  `.ai/policy.json` (red-zone rule never persisted back).
- `PermissionModal` now offers **Allow once / Allow for session / Always allow /
  Deny** (Enter = allow once, Esc = deny), with scope hints.
- `ask_approval` returns `AskOutcome` so dispatch distinguishes granted scopes,
  declined, timed-out and aborted for both memory and the audit trail.

### Changed files (this pass)
- `src-tauri/src/engine.rs` — `InferenceDone` breakdown + `outcome`;
  `run_generation` local accounting; FakeGen test updated
- `src-tauri/src/remote.rs` — `RemoteUsage`, `usage_from_chat` /
  `usage_from_anthropic`, `send_with_retry` + `backoff` + `retriable`,
  90s stall watchdog in both streamers
- `src-tauri/src/agent/orchestrator.rs` — breakdown aggregation through
  `FocusOutcome`/`SubResult`/`finish_outcome` + `outcome` mapping
- `src-tauri/src/agent/mod.rs` — `PermissionDecision`, `ToolState.session_allow`,
  permission channel type
- `src-tauri/src/agent/policy.rs` — `session_key` / `remember_session` /
  `remember_always`; session memory consulted in `check`
- `src-tauri/src/agent/tools.rs` — `AskOutcome` + `ask_approval` rewrite,
  dispatch decision wiring, `audit` (JSONL)
- `src-tauri/src/main.rs` — `agent_respond_permission(decision)`,
  `agent_audit_log` command, registered both
- `src/types.ts` — `InferenceDone` fields + `ChatMessage.done`, `AuditEntry`,
  `PermissionDecision`
- `src/lib/ipc.ts` — decision arg + `agentAuditLog`
- `src/components/PermissionModal.tsx` — 4 decision buttons
- `src/components/ChatPanel.tsx` — `OutcomeBadge` + `outcomeLabel`, footer
  token breakdown
- `src/components/AuditMenu.tsx` (new) + `src/components/StatusBar.tsx` —
  audit panel
- `src/App.tsx` — decision passthrough, store `done` on messages
- `PROJECT_STATUS.md` / `ROADMAP.md` — kept in sync

### Next step
Smoke test `npm run tauri:dev`: (a) run an agentic task → watch outcome badges,
per-turn token breakdown and the AuditMenu entries appear; (b) make a mutating
tool call → pick "Allow for session" then call again (no re-prompt), and "Always
allow" → confirm the rule lands in `.ai/policy.json`; (c) remote backend →
kill the network mid-stream → 90s stall abort + retry on startup. Then P1:
`create_plan`/`execute_plan`, goals & todos, first-class subagents.

---

- `cargo check` clean (zero warnings), `cargo test` **22/22** (1 ignored live-GGUF),
  `npm run build` green.

### What changed this pass (2026-08-15, parallel-threads + timeline pass)

**1. Engine pool — parallel agent threads, transmute removed**
- `engine.rs`: `StandaloneEngine._backend` is now `Arc<LlamaBackend>`;
  `LoadedModel` (`load_model_with_progress`) loads the GGUF once and shares it
  across N contexts via `new_engine_with_threads(threads)` (compute threads
  split across workers).
- New `EnginePool` / `EngineWorker` / `PoolGenerator`: each worker owns a
  generator on its own native thread for its whole life; `PoolGenerator`
  implements `TextGenerator` by message-passing over crossbeam channels.
  `EnginePool::drop` signals `EngineMsg::Stop` + joins (contexts released
  before the model drops). No `'static` transmute of the engine anywhere.
- `main.rs`: `InferenceState.engine` → `pool: Mutex<Option<Arc<EnginePool>>>`.
  `build_local_pool` (workers = `ModelInitParams.n_workers`, default **2**,
  clamp 1..=8; local GGUF loaded once + per-worker contexts) and
  `build_remote_pool` (**4** remote clients). `stream_inference` /
  `agent_run_task` dispatch to `pool.handle(...)` on the worker thread; the
  only remaining transmute is the `ToolState` read-only reference (documented).

**2. Parallel sub-task execution (`orchestrator.rs`)**
- `run_agent_loop_pool(pool, …)` (old single-generator `run_agent_loop`
  removed): sequential phases drive `pool.handle(0)`; a decomposed task with
  >1 subtask and >1 worker runs subtasks **concurrently** via
  `std::thread::scope` — one `pool.handle(i)` + one current-thread tokio
  runtime per subtask thread, each emitting its own running/done/failed
  `SubtaskStat` + steps. Results are merged as synthesized "Completed:
  Subtask i/n · title — done/failed" system messages before the shared summary
  turn. Single-worker runs fall back to the identical sequential loop.

**3. Step-group timeline**
- Every `StepStat` now carries a `group` phase label: `"Plan"` (plan mode),
  `"Execute"` (flat), `"Subtask i/n · title"` (decompose).
- `types.ts` `StepStat.group` + `StepTimelineStep` + `ChatMessage.steps?`;
  `App.tsx` appends each `agent-step` to its session's message.
- `ChatPanel.tsx` new `StepTimeline` component: groups steps by phase label
  (first-seen order), collapsible header showing `N steps · X tok · Y tool(s)`,
  rows `#n · tok · ms · tool(s)`. Single-phase turns collapse to one header.

### Changed files (this pass)
- `src-tauri/src/engine.rs` — `LoadedModel`, `load_model_with_progress`,
  `EngineMsg`/`EnginePool`/`PoolGenerator`/`EngineWorker` + `Drop` join,
  `ModelInitParams.n_workers`, `StepStat.group`, pool unit test (+1);
  removed pre-pool single-context helpers
- `src-tauri/src/agent/orchestrator.rs` — `run_focused_steps(group)` labels,
  new `run_agent_loop_pool` with parallel `std::thread::scope` subtasks,
  removed `run_agent_loop`
- `src-tauri/src/main.rs` — `InferenceState.pool`, `build_local_pool` /
  `build_remote_pool`, pool-based `stream_inference` / `agent_run_task`
- `src/types.ts` — `StepStat.group`, `StepTimelineStep`, `ChatMessage.steps?`
- `src/App.tsx` — append steps to message timeline in `onStep`
- `src/components/ChatPanel.tsx` — `StepTimeline` grouped/collapsible UI
- `PROJECT_STATUS.md` / `ROADMAP.md` — kept in sync

### Next step
Smoke test `npm run tauri:dev`: (a) `/plan` then Approve → watch "Plan" phase
steps, then "Subtask i/n · title" steps render concurrently with tool cards;
(b) `/decompose` a multi-part task → confirm parallel subtask chips and grouped
timeline; (c) remote backend → 4 workers. Then the remaining roadmap items:
**diff preview UI** and **session resume** (see ROADMAP).

---

## ✅ SHIPPED — checkpoints/undo UI + cost/token ledger + auto-checkpoint on plan approve, builds green

- `cargo check` clean, `cargo test` **21/21**, `npm run build` green.

### What changed this pass (2026-08-15, checkpoint & ledger pass)

**1. Direct git checkpoint / revert commands (bypass the agent tool loop)**
- `tools.rs`: `git_checkpoint` / `git_revert` now `pub`; new `git_checkpoints`
  helper lists tagged `checkpoint:` commits (hash/subject/relative age, newest
  first) for the UI.
- `main.rs`: new `#[tauri::command]`s `agent_git_checkpoint_cmd`,
  `agent_git_checkpoints_cmd`, `agent_git_revert_cmd` (registered in
  `invoke_handler`). They call the real tool fns with `InterruptState::current()`
  as the cancellation token. Revert is `reset --hard` — destructive, so the
  frontend confirms before calling.

**2. Checkpoint / one-click revert UI**
- New `src/components/CheckpointMenu.tsx`: a ↺ chip (with count) in the StatusBar
  opening a dropdown — "◆ Save checkpoint" on top, then the checkpoint list, each
  row one-click hard-resets the workspace to that commit (with `window.confirm`).
- `App.tsx` wires `createCheckpoint` / `revertToCheckpoint` / `refreshCheckpoints`;
  checkpoint list refreshes on workspace open and after every create/revert.
  Checkpoint/revert results are surfaced as timeline messages.

**3. Auto-checkpoint before destructive work**
- Approving a `/plan` now creates a checkpoint first (when a workspace is set),
  so the whole pre-approval state is always recoverable in one click.

**4. Aggregate cost/token ledger**
- New `types.ts` `LedgerEntry {sessionId, label, tokens, toolCalls, elapsedMs}`.
- `App.tsx` tracks one entry per session: tokens from `agent-step` `StepStat.tokens`
  (marked via `sessionHasStepsRef` so plain streaming falls back to
  `InferenceDone.totalTokens` — no double counting), tool calls from
  `agent-tool-event`, elapsed wall-time from session start.
- StatusBar now shows `Σ N sessions · X tok · Y tool(s) · Zms` with a per-session
  breakdown in the tooltip. Cleared with the chat.

### Changed files (this pass)
- `src-tauri/src/agent/tools.rs` — `pub` git_checkpoint/git_revert, new
  `git_checkpoints` helper
- `src-tauri/src/main.rs` — 3 new commands (checkpoint / checkpoints / revert)
- `src/types.ts` — `CheckpointInfo`, `ToolResultInfo`, `LedgerEntry`
- `src/lib/ipc.ts` — `gitCheckpoint` / `gitCheckpoints` / `gitRevert` wrappers
- `src/components/CheckpointMenu.tsx` — NEW
- `src/components/StatusBar.tsx` — ledger chip + checkpoint menu
- `src/App.tsx` — ledger state/refs, checkpoint handlers, auto-checkpoint on plan
  approve, ledger cleared on chat clear
- `PROJECT_STATUS.md` / `ROADMAP.md` — kept in sync

### Next step
Smoke test `npm run tauri:dev`: (a) open a workspace, save a checkpoint, make a
breaking edit, then one-click revert from the StatusBar menu; (b) run an agentic
task and watch the Σ session/token/tool ledger accumulate; (c) approve a `/plan`
and confirm a checkpoint is created automatically.

---

## ✅ BUILDING — sub-task decomposition (`/decompose`) shipped, builds green

- `cargo check` clean, `cargo test` **15/15**, `npm run build` green.
- **Chat verified live headlessly** against `D:\ai\models\qwen2.5-0.5b-instruct-q4_k_m.gguf`
  via an ignored test (`engine::tests::headless_chat_generation_streams_tokens`,
  run with `cargo test -- --ignored`): "hi" streamed 48 tokens, a second turn
  worked, and a **~870-token prompt decoded cleanly through the chunked batch
  path** (the old code failed here with "Insufficient Space of 512").
  `load_engine` was split into a thin AppHandle wrapper + `load_engine_with_progress`
  core so tests/tooling can load models without a running Tauri app.

### What changed this pass (2026-08-14, sub-task decomposition)
ROADMAP Tier 3 "sub-task decomposition": break a large request into focused
subtasks so a small local model never loses the thread of a big task.

**Backend (`orchestrator.rs`, `engine.rs`, `main.rs`)**
- `AgentTaskRequest.decompose` flag. When set (and not plan mode) the
  orchestrator runs three phases:
  1. **Plan** — one generation asks for a JSON subtask list
     (`[{"title", "instruction"}]`); output is parsed by `parse_subtask_plan`
     (JSON array, code-fenced or embedded, with a numbered-list fallback that
     only accepts lines starting with a digit). Parse failure → falls back to
     the flat loop, so the task always runs.
  2. **Execute** — each subtask runs its own `run_focused_steps` sub-loop
     (the existing generate → parse → dispatch → feedback loop, extracted from
     the old single loop) with the subtask instruction injected as
     `## Current subtask`. Subtasks run sequentially (one model, one engine);
     tool calls *within* a subtask still fan out via `join_all`. A failing
     subtask is recorded (`Subtask {status: failed}`) and the remaining
     subtasks continue; only an all-failed run reports `stop_reason = "stuck"`.
  3. **Summary** — a final plain-text generation (tools explicitly forbidden)
     becomes the user-facing report.
- New `WorkerEvent::Subtask` + `SubtaskStat {index, total, title, status}`
  → emitted as `agent-subtask` (`running` / `done` / `failed`).
- Plan mode now also rides the shared `run_focused_steps` (single step, no
  tools — unchanged behavior). The working-history budget guard from the
  previous pass is preserved and applied in every phase.
- Unit tests for `parse_subtask_plan`: JSON (incl. fenced/embedded), numbered
  fallback, garbage rejection. Backend tests 12 → 15.

**Frontend**
- `types.ts` `SubtaskStat`/`SubtaskEvent`; `events.ts` `onSubtask` handler;
  `ipc.ts` `agent-subtask` subscription + `AgentTaskRequest.decompose`;
  `useEngineEvents.ts` passthrough.
- `App.tsx`: `currentSubtask` state, `onSubtask` handler, `decompose` threaded
  through `runAgentTask`/`sendPrompt` (decompose forces the agent path).
- `ChatPanel.tsx`: new `/decompose <task>` slash command (+ hint chip) and a
  violet `subtask i/n` progress chip in the header while a task is decomposed.

### Changed files (this pass)
- `src-tauri/src/agent/orchestrator.rs` — `decompose` flag, `Subtask`,
  `parse_subtask_plan`, `run_focused_steps` extraction, `plan_subtasks`,
  `run_summary`, 3 new tests
- `src-tauri/src/engine.rs` — `SubtaskStat`, `WorkerEvent::Subtask`;
  `load_engine` split into AppHandle wrapper + `load_engine_with_progress` core;
  ignored live chat test (real GGUF, chunked-prompt regression)
- `src-tauri/src/main.rs` — `agent-subtask` emitter arm + `SubtaskEvent`
- `src/types.ts`, `src/lib/events.ts`, `src/lib/ipc.ts`,
  `src/hooks/useEngineEvents.ts`, `src/App.tsx`,
  `src/components/ChatPanel.tsx`
- `PROJECT_STATUS.md` / `ROADMAP.md` — kept in sync

### Next step
Smoke test `npm run tauri:dev` (chat + `/plan` + `/act` + `/decompose`), then:
1. **Diff preview UI** — render `file-changed.diff` inline in the timeline.
2. **Session resume** — replay `session_load` JSONL into the chat on workspace
   open (task memory across restarts).
3. **Semantic search (ROADMAP 2.8)** — embeddings index for code-aware recall.
4. **Parallel agent threads** — multiple model instances for true concurrent
   subtasks (needs engine-ownership rework; see ROADMAP constraint).

---

## ✅ BUILDING — runtime fixes: 512-token batch overflow + Tauri guard + prompt hardening, builds green

- `cargo check` clean, `cargo test` **12/12**, `npm run build` green.

### What changed this pass (2026-08-14, runtime-fix pass)
User hit "chat does some random action" while typing `hi`. Root causes found
and fixed:

1. **`Failed to queue prompt token: Insufficient Space of 512` (the real bug)**
   - `engine.rs` queued the **entire prompt** into a single
   `LlamaBatch::new(512, 1)` and decoded it in one call. Any prompt over 512
   tokens failed — and the expanded agent system prompt pushed every agent
   prompt over the limit, so even `hi` failed at "step 1".
   - Fixed: prompt is now **chunked into ≤512-token batches**, decoded
   sequentially; logits requested only on the final prompt token so generation
   starts from the correct KV position (`engine.rs` `run_generation`).
2. **Tauri runtime guard (earlier: `Cannot read properties of undefined (reading 'invoke')`)**
   - `lib/ipc.ts` now detects non-Tauri (browser) runs via `isTauriRuntime()`;
   every `invoke` goes through `tauriInvoke()` and surfaces a friendly error;
   `App.tsx` shows a banner "Not running inside the Tauri desktop shell. Launch
   with `npm run tauri:dev`". The app must never run via plain `npm run dev`.
3. **Spurious tool calls on greetings**
   - `lib/prompt.ts` hardened: greetings/small talk/general questions must be
   answered conversationally with NO `<execute_tool>`; tool use scoped to real
   workspace tasks; final summaries must not emit tool blocks.
4. **Mid-task context overflow (working-history budget)**
   - Agent working copy grows each step (assistant + tool feedback + heal
   injections); a long task could overflow the KV cache mid-task.
   - `orchestrator.rs`: `trim_working_history` now trims the working copy to the
   same 80% (`EVICTION_THRESHOLD`) of n_ctx before every prompt build, preserving
   pinned messages + the final message, oldest non-pinned dropped first.
   `run_agent_loop` takes a `context_budget` (n_ctx) threaded from `main.rs`
   (`engine.info().context_size`). +1 unit test.

### Changed files (this pass)
- `src-tauri/src/engine.rs` — chunked prompt decoding (batch-capacity fix)
- `src-tauri/src/agent/orchestrator.rs` — `trim_working_history` + budget guard + test
- `src-tauri/src/main.rs` — pass `context_budget` into `run_agent_loop`
- `src/lib/ipc.ts` — `isTauriRuntime()` / `tauriInvoke()` guard
- `src/App.tsx` — non-Tauri banner
- `src/lib/prompt.ts` — "when NOT to call tools" rules
- `PROJECT_STATUS.md` / `ROADMAP.md` — kept in sync

### Next step
`npm run tauri:dev` smoke test with a real local model (chat + `/plan` +
`/act`), then, in priority order:
1. **Parallel read-only fan-out verification** — confirm concurrent glob/read/
   search tools behave (already `join_all`, needs a real-model run).
2. **Sub-task decomposition** — allow the orchestrator to spawn parallel
   sub-agents (semantic/small-task parallelism) or sequential sub-plans.
3. **Semantic search (ROADMAP 2.8)** — embeddings index for code-aware recall.
4. **Diff preview UI** — render `file-changed.diff` in the editor/timeline.
5. **Session resume** — replay `session_load` JSONL into the chat on workspace
   open (task memory across restarts).

---

## ✅ BUILDING — "Go-to agentic tool" core shipped (search · skills · self-heal · sync · plan→act), builds green

- `cargo check` clean (zero errors/warnings), `cargo test` 11/11,
  `npm run build` green (tsc + vite).

### What changed this pass (2026-08-14)
Audited the whole stack for what a "go-to agentic tool" still lacked and
implemented, backend + frontend together:

**1. Code-content search — `search_file_contents` tool (new)**
   - Agent could only find files by *name*; now regex-searches *contents*
     (`src-tauri/src/agent/tools.rs` `search_file_contents`, `regex` crate,
     ignore-aware walk, include-glob filter, 512KB/2k-line caps, 200-result cap).
   - Classified read-only → default-allow in `policy.rs`.

**2. Self-development of skills — `create_skill` tool (new)**
   - Agent can persist a learned procedure to `{workspace}/.ai/skills/<slug>.md`
     (frontmatter name/description/created) and emits `agent://skills-changed`;
     the UI auto-rescans knowledge so the skill is live immediately.
   - Prompt now instructs the model to call `create_skill` when it discovers a
     reusable approach.

**3. Self-healing retry loop (orchestrator)**
   - `orchestrator.rs`: consecutive fully-failed tool steps now inject a
     SELF-ASSESSMENT system turn ("diagnose root cause, correct, don't repeat
     the identical call") up to 3×, and the loop aborts with `stop_reason =
     "stuck"` after 3 consecutive all-failed steps (bounded, no infinite burn).

**4. Live terminal streaming — `agent://tool-output` (new)**
   - `execute_terminal_command` (and `run_tests`) now stream stdout/stderr line
     by line to the UI while the command runs; the ToolResult still returns the
     full captured output. Per-line cap + 4k char scrollback on the card.

**5. Editor sync — `agent://file-changed` now wired**
   - Backend already emitted it; the frontend now listens and reloads any
     *open* file from disk so Monaco shows the agent's edits live
     (`App.tsx syncAgentFile`).

**6. Plan → Act workflow in the UI**
   - `/plan <task>` runs the backend's plan_mode (single step, no tools); the
     plan renders with **✓ Approve & Execute** / **✕ Reject** buttons.
     Approve re-invokes in agent mode with `verify: true`; the plan stays in
     context (new `## Approved plan` role header). Reject discards it.

**7. Verify (test-after-edit) toggle**
   - ChatPanel header "Verify" pill (default on, agent mode) → sets
     `agentRunTask.verify`, the backend nudges the model to run tests/typecheck
     after every successful file edit.

**8. Step timeline + slash commands**
   - `agent-step` events now render a "step N" chip while an agentic task runs.
   - Slash commands: `/plan /act /fix /test /commit /skills /clear` (+ inline
     hint chips when input starts with `/`).
   - `prompt.ts` rewritten: 11 tools documented incl. `search_file_contents`,
     `write_file`, `create_skill`, `run_tests`, git workflow; self-assessment
     rules added.

### Tool inventory (agent) — 15 total
`glob_search_codebase` · `search_file_contents` · `view_file_structure` ·
`read_file_range` · `apply_file_diff` · `write_file` · `execute_terminal_command`
(streaming) · `call_mcp_tool` · `run_tests` · `git_status` · `git_diff` ·
`git_commit` · `git_checkpoint` · `git_revert` · `create_skill`

### Changed files (this pass)
- Backend: `Cargo.toml` (+`regex`), `agent/mod.rs` (2 new ToolCall variants +
  summaries), `agent/tools.rs` (search + create_skill + streaming terminal),
  `agent/core.rs` (schemas), `agent/policy.rs` (default-allow for search),
  `agent/orchestrator.rs` (self-heal loop, plan role, stuck detection)
- Frontend: `types.ts`, `lib/events.ts`, `lib/ipc.ts`, `hooks/useEngineEvents.ts`
  (4 new subscriptions), `App.tsx` (editor sync, plan flow, verify, step chip,
  skills-changed rescan), `components/ChatPanel.tsx` (rewrite), `lib/prompt.ts`

### Next step
`npm run tauri:dev` smoke test, then, in priority order:
1. **Parallel read-only fan-out verification** — confirm concurrent glob/read/
   search tools behave (already `join_all`, needs a real-model run).
2. **Sub-task decomposition** — allow the orchestrator to spawn parallel
   sub-agents (semantic/small-task parallelism) or sequential sub-plans.
3. **Semantic search (ROADMAP 2.8)** — embeddings index for code-aware recall.
4. **Diff preview UI** — render `file-changed.diff` in the editor/timeline.
5. **Session resume** — replay `session_load` JSONL into the chat on workspace
   open (task memory across restarts).

---

## ✅ BUILDING — Tier 2.1 Safety/permissions + Tier 2.3 Memory/skills shipped, builds green

- `cargo check` clean (zero errors/warnings), `cargo test` 11/11,
  `npm run build` green (tsc + vite).

### This pass — completed the interrupted Tier 2.1/2.3 work
The previous session wrote the backend for permissions + skills but left it
**uncompiled (17 errors)** and the frontend completely unwired. Fixed and wired:

1. **Fixed 17 compile errors** across `policy.rs`, `skills.rs`, `tools.rs`,
   `main.rs`:
   - `policy.rs` — `matches!` pattern bindings don't escape the macro; rewrote
     the red-zone check as `if let`. Removed dead `deny_reason`; wired
     `default_allow` into `check` so read-only tools (glob/AST/read/git status/
     diff/tests) default to **allow** instead of the `ask` default.
   - `skills.rs` — used `tokio::sync::Mutex` in sync methods; switched to
     `std::sync::Mutex`, made `scan(&self)` (interior mutability), made `roots`
     a `Mutex` too.
   - `tools.rs` — `git_diff` awaited inside a non-async `.and_then` closure;
     rewrote sequentially. `similar` 2.7 API: unified diff builder now iterates
     via `.iter_hunks()` (was direct for-in).
   - `main.rs` — added `use tauri::Manager` for `app.path()`.
2. **Permission-approval flow wired into the UI** (`agent://permission-request`
   → `agent_respond_permission`):
   - New `PermissionModal.tsx` — modal with tool label, command summary,
     Allow/Deny, Esc=deny / Enter=allow, policy snapshot hint.
   - `events.ts`/`ipc.ts`/`useEngineEvents.ts` — new `onPermission`/`onKnowledge`
     subscriptions + IPC wrappers (`agentRespondPermission`, `agentPolicySnapshot`).
   - App shows the modal; StatusBar keeps a live policy snapshot.
3. **Skills & Rules panel** (`KnowledgePanel.tsx`, opened via ✦ in the Explorer
   header): lists rules (AGENTS.md/.cursorrules/CLAUDE.md/.ai/rules), toggles
   skills (`.ai/skills/*.md`, frontmatter name/description), ↻ Rescan →
   `knowledge_scan`; active skills are synced into the pinned context buffers.
4. **Settings persistence** — `settings_load`/`settings_save` now persist the
   gen params + last remote connection (provider/baseUrl/model — **never the
   API key**), prefilled into the ModelBar "Remote…" popover next launch.
5. **Session history** — user/assistant turns appended to the project JSONL log
   (`session_append`) keyed by workspace root.

### Next step
`npm run tauri:dev` smoke test: (a) agent mode with a remote/8B+ model → verify
streaming, tool cards, tool-call loop, and the new permission modal (file edits
should now pop the Allow/Deny dialog); (b) local GGUF still streams. Then
roadmap step 2.2: **Plan → Act separation**.

---

## ✅ BUILDING — Tier 1 SHIPPED (1.1–1.3) + Model Router (1.4) DONE

- Verified: `cargo check` clean (zero errors/warnings), `cargo test` 9/9,
  `npm run build` green (tsc).
- `ROADMAP.md` added — full super-agentic analysis + Tier 1/2/3 plan.

### Shipped — agentic pass
1. **Session-id plumbing** — `WorkerEvent` carries `session_id`
   (`Token/Done/Error` variants); `spawn_emitter` uses real ids (was hardcoded
   0 while `next_session()` is 1-based → live streaming was keyed wrong).
   `run_generation` returns `GenerationOutcome { done, full_text }`.
2. **Agent orchestrator** — `agent/orchestrator.rs`: `run_agent_loop` builds a
   prompt from the `ContextManager` snapshot → generate (streams tokens) →
   parses `<execute_tool>` → `tools::dispatch` (circuit breaker armed) →
   appends tool feedback → repeats up to `max_steps` (default 6). Aggregated
   stats via `inference-done`. Command `agent_run_task`. 3 unit tests.
3. **Context wiring** — system prompt (`src/lib/prompt.ts`) set on model load;
   active-file buffer pushed (debounced 800ms, capped 8k chars); workspace
   synced to `agent_set_workspace`.
4. **Save-as** — `save_file_as` command (native dialog).
5. **UI feedback** — ChatPanel renders live `agent://tool-event` cards; "Agent"
   mode toggle routes sends to `agent_run_task`; `execution-aborted` surfaces
   in the status bar.

### Shipped — model router (ROADMAP 1.4)
- `engine.rs` — new `TextGenerator` trait (info + generate) + `LocalGenerator`
  wrapping `StandaloneEngine`; `InferenceState.engine` is now
  `Mutex<Option<Box<dyn TextGenerator>>>`. `stream_inference` + `agent_run_task`
  speak only to the trait.
- `src-tauri/src/remote.rs` — `RemoteGenerator`: OpenAI-compatible
  `POST {base}/chat/completions` with SSE streaming (reqwest + futures-util),
  circuit-breaker abort drops the HTTP stream, token estimate fallback, memory-
  only API key. Command `configure_remote_model`. Dependencies: `reqwest`
  (json/stream/rustls-tls), `futures-util`.
- Frontend — ModelBar "Remote…" connect popover (base URL / API key / model)
  + local/remote badge in the model line; `api.configureRemoteModel` wired.
  Works with OpenAI, Ollama `/v1`, LM Studio, vLLM, Anthropic-compatible
  gateways (delta paths: `choices[0].delta.content`, `choices[0].text`,
  `content_block_delta`).

### Next step
Smoke-test: (a) remote mode with any OpenAI-compatible endpoint + Agent toggle
→ verify streaming, tool cards, tool-call loop; (b) local GGUF still works.
Then roadmap step 3: safety/permission model + plan mode.

---


## ✅ BUILDING — `cargo check` passes (BUILD_EXIT=0), frontend `npm run build` green

- Re-verified this session: `npm run build` green (tsc clean), `build.ps1` →
  `BUILD_EXIT=0` (3 pre-existing warnings in `agent/core.rs` only), 6 unit tests
  pass (`agent::context` + `agent::core`).
- GGUF model present at `D:\ai\models\qwen2.5-0.5b-instruct-q4_k_m.gguf`.

### Fixed this session
- **KV off-by-one** in `engine.rs` `run_generation`: generated tokens were
  placed at `prompt_len+1` instead of `prompt_len`, so llama.cpp decode failed
  with "inconsistent sequence positions" (X=0, Y=2). Now `n_cur` starts at
  `prompt_len` and increments *after* placing each token (both normal and
  empty-piece branches).

### Added this session (hardening)
- `src-tauri/src/agent/context.rs` — `ContextManager`: token tracking via the
  Hugging Face `tokenizers` crate (fancy-regex backend) with a 4-chars/token
  heuristic fallback; 80% sliding-window eviction evicts oldest turns while
  pinning system prompt + active-file buffer. Commands: `context_status`,
  `context_set_system_prompt`, `context_set_file_buffer`, `context_push_turn`.
  Model load aligns the budget with `info.context_size`.
- `src-tauri/src/agent/interrupt.rs` — circuit breaker wrapping
  `tokio_util::sync::CancellationToken`; re-armed per generation. Replaced the
  old `AtomicBool` cancel flag in `run_generation`/`InferenceState`. Command
  `abort_agent_execution` returns + emits an "Execution Aborted" payload.
  Terminal sub-processes and MCP calls race against the token and kill/clean up
  on abort.
- `src-tauri/build.rs` — hardware autodetection: macOS → `ai_accel_metal` cfg +
  Metal auto-wired via `[target.'cfg(target_os="macos")']` llama-cpp-2 entry;
  Windows/Linux → NVCC/CUDA-path scan → `ai_accel_cuda` cfg + guidance. GPU
  backends: `gpu-cuda` / `gpu-metal` Cargo features (build.rs fast-fails if a
  GPU feature is set without a toolchain).
- `src/components/InterruptButton.tsx` — global stop button + double-Esc
  shortcut, wired to `abort_agent_execution`. StatusBar shows a live
  `ctx <used>/<limit>` gauge (amber while evicting).

Next step: `npm run tauri dev` to relaunch, then smoke-test streaming again
(the KV fix should resolve the previous decode error) and the new abort path.

---

## What this project is

`D:\ai` — **AI Editor**: an ultra-fast, low-memory, fully **local/offline** AI
code editor with 21 agentic tools.

- Frontend: React 19 + Vite 6 + Tailwind v4 + Monaco editor
- Desktop shell: Tauri 2 (Rust) — frameless window, host-side fs + model I/O
- Local inference: `llama-cpp-2` (Rust bindings to llama.cpp) — CPU-only build,
  GGUF models
- Streaming: Rust worker thread → bounded crossbeam MPSC → Tauri events →
  rAF-batched React state
- Agent: orchestrator (generate → parse → dispatch → feedback, plan mode,
  parallel subtasks via engine pool, self-healing retry) + 21-tool layer

## Completed

### Toolchain (installed this session)
- Node v24.12.0, npm 11.9.0 (pre-existing)
- Rust **stable 1.97.1** via rustup (winget `Rustlang.Rustup`) at
  `C:\Users\durga\.cargo\bin` — **cargo/rustc NOT on PATH for new shells**;
  prepend `$env:USERPROFILE\.cargo\bin` each session
- Visual Studio Build Tools 2022 (17.14.37) at
  `C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools`
  - MSVC toolchain 14.44.35207 (`VC\Tools\MSVC\14.44.35207`)
  - Windows 11 SDK (26100) — verified via vswhere
- CMake bundled in VS at:
  `...\BuildTools\Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin`
  (NOT on PATH — add to PATH for cargo builds)
- **libclang.dll solved** via portable LLVM extraction (installers kept failing):
  - Extracted the LLVM 22.1.8 win64 NSIS installer (downloaded to
    `%TEMP%\LLVM-install.exe`) with 7-Zip (`C:\Program Files\7-Zip\7z.exe`)
  - → `C:\Users\durga\AppData\Local\LLVM-portable\` (contains `bin\libclang.dll`
    + `lib\clang\22\include` builtin headers)
  - Set `LIBCLANG_PATH = C:\Users\durga\AppData\Local\LLVM-portable\bin`
  - (VS BuildTools 17.14.37's installer kept exiting 87 / hanging; the abandoned
    `VC\Tools\Llvm` partial dir is harmless)

### Rust backend
- `cargo check` in `src-tauri` passes with **zero errors** (llama.cpp compiled
  fully: 233 obj files; 460+ dep crates).
- Fixed in compile loop: `DialogExt` import, `Default` derive on `ModelInitParams`,
  `StartedEvent/TokenEvent/DoneEvent/ErrorEvent` derive `Clone`, `fn main()` stub,
  `FilePath::into_path()`, `FileDialogBuilder::blocking_pick_folder()`,
  `state.cancel` is a shared `Arc<AtomicBool>` (reset to false per generation —
  no swap needed, avoids Tauri-2 `State` being read-only), `app` clone before
  `spawn_blocking`, CP1252 fallback match, `stop_reason` via `loop { break "x" }`.
- Helper script `src-tauri/build.ps1` runs `cargo check` in background with the
  right PATH + LIBCLANG_PATH, logging to `src-tauri\build.log` (ends with
  `BUILD_EXIT=<code>`). Reuse for future builds.

### Scaffold / config
- `package.json`, `vite.config.ts`, `tsconfig.json`, `index.html`, `.gitignore`
- Tailwind v4 via `@tailwindcss/vite` + `@theme` tokens in `src/index.css`
- `src-tauri/tauri.conf.json` (frameless, CSP, icons), `build.rs`,
  `capabilities/default.json`
- App icon generated (`app-icon.png` → `src-tauri/icons/*` via `npx tauri icon`)

### Rust backend (`src-tauri/src/`)
- `engine.rs` — `StandaloneEngine` (ctx+model+backend), `load_engine`
  (spawn_blocking, progress events), `run_generation` (token loop, stop words,
  stop reasons, tok/s stats), `build_sampler`
- `main.rs` — `InferenceState` (tokio Mutex, cancel flag, session_id),
  emitter thread, commands:
  `pick_and_load_model`, `unload_model`, `model_status`, `stream_inference`,
  `cancel_inference`, `pick_workspace_folder`, `list_directory`,
  `read_text_file`, `write_text_file`
- Pinned `llama-cpp-2 = "=0.1.154"` with `default-features = false`,
  `features = ["common", "sampler"]` (0.1.154 is the latest published version;
  crate is non-semver — do NOT bump blindly)
- `[profile.release]` opt-level 3, lto, codegen-units 1

### Frontend (`src/`)
- `main.tsx`, `App.tsx` (layout + state orchestration)
- `lib/ipc.ts` (typed invoke wrappers + event subscriptions),
  `lib/events.ts`, `lib/monaco.ts` (local Monaco + bundled workers — no CDN)
- `hooks/useTokenStream.ts` (rAF batched stream), `hooks/useEngineEvents.ts`,
  `hooks/useDebouncedResize.ts`
- `components/`: `TitleBar`, `ModelBar`, `FileExplorer`, `EditorPane` (Monaco),
  `Tabs`, `ChatPanel`, `StatusBar`
- `npm run build` passes: `tsc` clean + `vite build` OK

### Toolchain (installed this session)
- Node v24.12.0, npm 11.9.0 (pre-existing)
- Rust **stable 1.97.1** via rustup (winget `Rustlang.Rustup`) at
  `C:\Users\durga\.cargo\bin` — **cargo/rustc NOT on PATH for new shells**;
  prepend `$env:USERPROFILE\.cargo\bin` each session
- Visual Studio Build Tools 2022 (17.14.37) at
  `C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools`
  - MSVC toolchain 14.44.35207 (`VC\Tools\MSVC\14.44.35207`)
  - Windows 11 SDK (26100) — verified via vswhere
- CMake bundled in VS at:
  `...\BuildTools\Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin`
  (NOT on PATH — add to PATH for cargo builds)

## In progress / next

- ~~BLOCKED: bindgen/libclang~~ → **RESOLVED** (see Toolchain above).
- **P1-6 shipped**: persistent `create_plan`/`execute_plan` with per-item focused
  loops, `read_plan`, `update_plan`, `.ai/plan.json` + `.ai/plan.md` persistence,
  `agent://plan-step` events, system prompt updated.
- **Next P1 items** (highest ROI first):
  - **P1-7: Goals & todos** — `create_goal`/`update_goal`/`read_todos`/`update_todos`
    for tracking progress across sessions; derives from the plan state.
  - **P1-8: First-class subagents** — `task` tool with `subagent_type`
    (`EXPLORE|BASH|DEBUG|CUSTOM`), per-child restricted `permission_mode`,
    `subagent_await` for async spawn/await (currently: parallel decompose only).
  - **P1-9: `ask_question` / `send_to_user`** — async human interaction mid-task;
    the agent pauses and waits for the user to answer a clarifying question.
  - **P1-10: Git toolchain** — `blame`, `push`, `pull`, `create_branch`,
    `create_pr`, `pr_status`, `ci_status` (§7 Git/CI/PR).
  - **P1-11: File tool gaps** — `delete`; `read_lints`/`diagnostics` (tree-sitter
    backed, §7 Files).
- **Smoke test**: `npm run tauri:dev` with a model that exercises the plan tools
  end-to-end.

## Pending / next steps (ordered)

### Immediate — smoke test
1. `npm run tauri:dev` (compiles the dev binary + links ~1-3 min, then opens the
   window). Command: `npm run tauri:dev` from `D:\ai`.
2. Smoke-test the full flow: load model → chat → agent task → verify plan tools
   work end-to-end (model calls `create_plan` → `update_plan` → `execute_plan`).
3. `npm run tauri build` later for a production bundle (release build is slow;
   uses opt-level 3 + lto).

### P1 — agent capabilities (next implementation target)
4. **P1-7: Goals & todos** — `create_goal`/`update_goal`/`read_todos`/`update_todos`
   for tracking progress across sessions; derives from the plan state.
5. **P1-8: First-class subagents** — `task` tool with `subagent_type`
   (`EXPLORE|BASH|DEBUG|CUSTOM`), per-child restricted `permission_mode`,
   `subagent_await` for async spawn/await.
6. **P1-9: `ask_question` / `send_to_user`** — async human interaction mid-task;
   the agent pauses and waits for the user to answer a clarifying question.
7. **P1-10: Git toolchain** — `blame`, `push`, `pull`, `create_branch`,
   `create_pr`, `pr_status`, `ci_status`.
8. **P1-11: File tool gaps** — `delete`; `read_lints`/`diagnostics` (tree-sitter
   backed).

### P2 — concurrency & UX
9. **P2-12: Background work & multitasking** — `spawn_background_shell` /
   `background_subagent` that survive turn end, pill/badge UI, `abort_background_work`.
10. **P2-13: Session management UI** — `list_sessions`, `fork_session`, watch
    lifecycle, statuses `AWAITING_INPUT|ERROR|ABORTED`.
11. **P2-14: Modes** — `ASK` (every tool prompts), `DEBUG`, `CUSTOM` (per-mode
    system prompt + tool allowlist), `switch_mode`.

### P3 — scale & polish
12. **P3-15: Context compaction** — at ~80% context summarize older messages into
    a `ConversationSummaryArchive` instead of hard-evicting.
13. **P3-16: Context usage tree + blob store** — per-component token contribution
    + blob store for large context.
14. **P3-17: Smart-mode classifier** — lightweight local risk classifier +
    natural-language `allow_instructions`/`block_instructions`.

## API notes (verified against llama-cpp-2 0.1.154 docs.rs)

- `LlamaBackend::init()`; `LlamaModel::load_from_file(&backend, path, &params)`
- `LlamaModelParams::default().with_n_gpu_layers(u32).with_use_mmap(bool)
  .with_progress_callback(|f32| bool)`
- `LlamaContextParams::default().with_n_ctx(NonZeroU32).with_n_threads(i32)
  .with_n_threads_batch(i32).with_n_batch(usize)`
- `model.new_context(&backend, ctx_params) -> Result<LlamaContext>`
- `LlamaContext` is **`!Send` + `!Sync`**; code works around via `unsafe impl
  Send` on the wrapper + a tokio Mutex + worker-thread `blocking_lock` +
  `mem::transmute<'static>` (sound: llama.cpp calls are serialized)
- `ctx.clear_kv_cache()` exists (per-run cache reset), `ctx.reset_timings()`,
  `ctx.n_ctx() -> u32`, `ctx.decode(&mut LlamaBatch)`, `ctx.n_batch()`
- `batch.add(token: LlamaToken, pos: i32, seq_ids: &[i32], logits: bool)`
  (logits = `is_last`)
- `model.str_to_token(&str, AddBos::Always)`; `model.token_to_piece(token,
  &mut Decoder, bool, None)`; `model.is_eog_token(token)`; `meta_val_str`
- `LlamaSampler::chain_simple(Vec<LlamaSampler>)`,
  `temp(f32)`, `top_p(f32, 1)`, `greedy()`, `dist(seed)`; last sampler must be
  token-selecting; `sampler.sample(&ctx, idx)`, `sampler.accept(token)`
- llama-cpp-2 features (0.1.154): `common`, `sampler`, `cuda`, `openmp`,
  `metal`, `vulkan`, `dynamic-backends`... `default = [openmp, common,
  android-shared-stdcxx]`. We deliberately use **no openmp** (CPU-only, avoids
  vcomp link issues).

## Project layout

```
D:\ai
├─ index.html, package.json, vite.config.ts, tsconfig.json, .gitignore
├─ app-icon.png            (source icon; icons in src-tauri/icons)
├─ src/                    (React frontend)
│  ├─ components/          (ChatPanel, StatusBar, EditorPane, PermissionModal,
│  │                       KnowledgePanel, CheckpointMenu, AuditMenu, etc.)
│  ├─ hooks/               (useTokenStream, useEngineEvents, useDebouncedResize)
│  └─ lib/                 (ipc, events, monaco, prompt, settings)
└─ src-tauri/
   ├─ Cargo.toml, build.rs, tauri.conf.json, capabilities/default.json, icons/
   └─ src/
      ├─ engine.rs          (GGUF pool, TextGenerator trait, streaming)
      ├─ remote.rs          (OpenAI-compatible SSE, stall watchdog, retries)
      ├─ main.rs            (Tauri commands, InferenceState, pool management)
      └─ agent/
         ├─ mod.rs           (ToolCall enum, ToolState, PlanStepEvent, PermissionDecision)
         ├─ orchestrator.rs  (generate → parse → dispatch → feedback loop, plan/subtask/decompose)
         ├─ tools.rs         (21 tool implementations + dispatch + audit)
         ├─ core.rs          (JSON schemas, <execute_tool> parser)
         ├─ policy.rs        (allow/ask/deny, red-zone, workspace scoping, decision memory)
         ├─ context.rs       (token tracking, 80% sliding-window eviction)
         ├─ plan.rs          (PlanState, PlanStatus, .ai/plan.json + .ai/plan.md)
         ├─ skills.rs        (rules + toggleable skill bundles)
         ├─ interrupt.rs     (circuit breaker, CancellationToken)
         └─ mcp.rs           (minimal stdio JSON-RPC MCP client)
```

## Session environment gotchas

- Shell is **pwsh 7 on win32**; workdir `D:\ai`
- cargo/rustc need `$env:Path` prepend every new shell
- CMake only inside VS dir (add to PATH for cargo)
- `npm run build` runs `tsc && vite build` (typecheck gate)
- Installers that need UAC may be silently canceled in this session — prefer
  approaches that run detached/quiet and poll for artifacts
