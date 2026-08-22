# Roadmap — From "local chat + editor" to "super agentic coding agent"

_Last updated: 2026-08-22 (boot-smoke root cause fixed — `custom-protocol`
feature; smoke + live-GGUF verified end-to-end. Earlier: Bionic backlog
BN-2…BN-8 verified
complete — todos, web tools, sandboxed code execution, Coder/YOLO hardening,
skills v2, MCP catalog manager w/ env + allowed-tools filtering, model hub +
local OpenAI-compatible API server. BN-9/BN-10 partial: neural embedder and
Voice Keyboard overlay remain).
Strategic plan derived from a full codebase audit (`src/` +
`src-tauri/src/`). Companion to `PROJECT_STATUS.md` (session status / source of
truth for what has shipped)._
_Tick marks (`[x]`) = shipped; `[ ]` = remaining._

## Where we are today

Solid engineering skeleton: streaming llama.cpp + OpenAI-compatible remote
engines, an agent orchestrator (generate → parse → dispatch → feedback, plan
mode, parallel `join_all` dispatch, self-healing retry), a 17-tool layer
(glob, content-search, semantic-search, AST, read, diff-apply, write, streaming
terminal, MCP, tests, git ×5, create-skill, read-skill), a circuit breaker, a
context eviction engine, permissions/policy with decision memory, audit logging,
skills & rules, editor sync, diff preview, session resume, Plan→Act UI, slash
commands, and a clean Tauri 2 /
React 19 shell.

## Tier 1 — Foundations

| # | Gap | Status |
|---|-----|--------|
| 1.1 | **Agent orchestration loop** — generate → parse `<execute_tool>` → dispatch → feed result back → repeat | [x] shipped (`orchestrator.rs`) |
| 1.2 | **Context actually reaching the model** | [x] shipped (`context.rs`, active-file buffer, rules, skills) |
| 1.3 | **Session-id correctness** | [x] shipped |
| 1.4 | **Capable model routing** | [x] shipped (`remote.rs` OpenAI-compatible SSE) |

## Tier 2 — What makes Cursor / Claude Code feel "super"

1. **Safety & permission model** — per-tool policy (`allow` / `ask` / `deny`),
   workspace scoping, command allowlist, red-zone commands, sandboxed execution. [x] shipped
2. **Plan → Act separation** — model produces a plan first, user approves,
   execution mode runs it. [x] shipped (`/plan`, Approve & Execute / Reject in UI)
3. **Memory & persistence** — conversation history JSONL per project, project
   instructions (`.cursorrules` / `AGENTS.md` / `.ai/rules`), skills
   (`.ai/skills`), session resume. [x] shipped — history + instructions + skills
   + **session resume** (JSONL replayed into the chat + model context on
   workspace open).
4. **Git-native workflow** — `git status` / `git diff` tools, checkpoint before
   edits, commit, revert. [x] shipped (5 git tools) — auto-checkpoint/undo
   buttons now in UI (**CheckpointMenu.tsx** in the StatusBar: save checkpoint,
   one-click hard-reset revert with confirm; auto-checkpoint before executing an
   approved plan).
5. **Test-driven feedback loop** — auto-run tests/typecheck after edits and
   self-correct on failures. [x] shipped (`run_tests`, verify flag, `auto-verify`
   nudge after edits).
6. **Self-healing retry** — on tool failure, feed the error back and retry with
   a bounded budget instead of dying. [x] shipped (SELF-ASSESSMENT injection,
   `stuck` stop-reason after 3 consecutive all-failed steps).
7. **Parallel tool execution** — parallel read-only fan-out cuts latency. [x]
   shipped (`join_all`) — needs a real-model latency validation run.
8. **Semantic codebase search** — embeddings index + ranking (glob + AST is not
   "find where the auth logic is"). [x] shipped — `semantic_search_codebase`
   tool: local code-aware TF-IDF index over sliding line windows, ranked by
   cosine similarity (no external model; fully offline). Interim content regex
   search remains as the exact-match tool.
9. **File-write preview & editor sync** — diffs are applied to disk but the open
   Monaco editor never refreshes; need diff preview + reload. [x] shipped —
   editor reload (`agent://file-changed`) **and** inline diff preview
   (`DiffView.tsx`, rendered in the chat timeline with add/del counts).
10. **Live tool output streaming** — terminal stdout arrives only on completion.
    [x] shipped (`agent://tool-output`, line streaming).
11. **Token / cost & step telemetry** — per-step tokens, tool durations, context
    pressure. [x] shipped — `agent-step` events carry per-step tokens/duration/
    tool count; the chat timeline now **groups steps by phase** (Plan / Execute /
    Subtask i/n · title) in collapsible sections; aggregate cost/token ledger in
    the StatusBar (`Σ N sessions · X tok · Y tool(s) · Zms`, per-session
    breakdown in tooltip; tokens from step events, plain streaming falls back to
    `inference-done.totalTokens`).
12. **Turn lifecycle outcomes** — `InferenceDone.outcome`
    (`completed | failed | interrupted | error`) derived from stop reason /
    stuck / cancel, rendered as per-turn badges + footer label. [x] shipped
    (P0-1)
13. **Token accounting breakdown** — `InferenceDone` carries
    `input / output / cache_read / cache_write / reasoning_tokens` (§4
    `turn_ended`); local = honest (cache write = prompt, read = 0, KV cleared
    per run), remote = parsed provider `usage`. [x] shipped (P0-2)
14. **Remote stall detection + retry** — 90s per-chunk stall watchdog aborts
    silent streams; transient request errors (connect/408/429/5xx) retry with
    exponential backoff (max 2), pre-stream only. [x] shipped (P0-3)
15. **Audit log** — every tool-call policy verdict persisted to
    `.ai/audit.jsonl` (`tool, summary, decision, latency, success, error`) with
    a status-bar **AuditMenu** panel (§0 non-negotiable). [x] shipped (P0-4)
16. **Permission decision memory** — `allow_once / allow_session /
    always_allow / deny`; session memory in `ToolState` (exact-command match for
    terminal), `always_allow` persists to `.ai/policy.json`. [x] shipped (P0-5)

## Tier 3 — Product polish

- Agent **timeline UI**: collapsible step/tool cards with status, duration,
  expandable stdout. [x] shipped — tool cards + live stdout + per-phase
  **step-group timeline** (`StepTimeline` in ChatPanel, collapsible group
  headers with step/token/tool totals).
- **Checkpoints / snapshots** + one-click revert. [x] shipped —
  direct `agent_git_checkpoint_cmd` / `agent_git_revert_cmd` / list commands,
  **CheckpointMenu** in the StatusBar (save + one-click revert w/ confirm),
  auto-checkpoint before executing an approved plan.
- **Slash commands + command palette** (`/fix`, `/test`, `/commit`), plan/act
  toggle. [x] shipped (`/plan /act /fix /test /commit /skills /clear`).
- Model **settings UI** persisted. [x] shipped (gen params + remote prefill via
  `settings_load`/`save`).
- **Sub-task decomposition** (`/decompose`) — plan → per-subtask focused loops →
  summary. [x] shipped — subtasks now run **concurrently** on an engine pool
  (one worker per subtask, compute threads split across workers; sequential
  fallback when fewer workers than subtasks).

## Recommended architecture (fits current stack, no rewrite)

```
┌─────────────────────────────────────────────┐
│ Frontend: Chat timeline, tool cards, diff UI │
└──────────────┬──────────────────────────────┘
               │ invoke / events (session-scoped)
┌──────────────▼──────────────────────────────┐
│ Orchestrator (async, circuit-breaker armed) │  ← Tier 1
│  loop: prompt → stream → parse tools →      │
│        dispatch (parallel) → feedback       │
└──────┬──────────────┬──────────────┬────────┘
       │              │              │
┌──────▼─────┐ ┌──────▼──────┐ ┌─────▼────────┐
│ Context    │ │ Tool layer  │ │ Model router │  ← Tier 1–2
│ Manager    │ │ (+safety,   │ │ local GGUF / │
│ (prompt    │ │  git, test, │ │ OpenAI-compl │
│  assembler)│ │  retry)     │ │ per-planning │
└────────────┘ └─────────────┘ └──────────────┘
       ┌───────────────┴───────────────┐
       │ Backends: KV cache across steps│  ← big perf win
       │ embedding index, persistence   │
```

**Critical constraint — RESOLVED**: the old single-engine architecture
transmuted a `&'static mut` engine onto a worker thread per generation. That
was replaced by an **engine pool** (`EnginePool` in `engine.rs`): the GGUF loads
once into a shared `LoadedModel`, and N long-lived worker threads each own their
own context/client for their whole life. Callers hold cloneable `PoolGenerator`
handles that proxy over channels. There is **no engine transmute** anywhere; the
only remaining transmute is a read-only `ToolState` reference (documented
SAFETY). This unblocks true parallel subtasks and lets one model serve several
concurrent generations.

## Implementation order (highest ROI first)

1. **Agent loop + context feeding + session-id fix** (Tier 1.1–1.3) — ✅ shipped
2. **Model router** (Tier 1.4) — ✅ shipped
3. **Permission/safety + plan mode** (Tier 2.1–2.2) — ✅ shipped
4. **Git checkpoints + retry + tests loop** (Tier 2.4–2.6) — ✅ shipped
5. **Semantic search + parallel tools + timeline UI** (Tier 2.7–2.11) — ✅
   parallel tools + timeline + editor sync + live streaming shipped; **semantic
   search** (`semantic_search_codebase`, local TF-IDF index) shipped.
6. **Sub-task decomposition** (Tier 3) — ✅ shipped (`/decompose`: model emits a
   JSON subtask list; each subtask runs a focused agent loop with its own step
   budget and step-group label; a final plain-text summary closes the task;
   failing subtasks are recorded and the rest continue). **Parallel agent
   threads** ✅ shipped — engine pool (`EnginePool`/`PoolGenerator`, no engine
   transmute); decomposed tasks with spare workers run subtasks concurrently via
   `std::thread::scope`, one engine worker per subtask.
7. **Diff preview UI + session resume** — ✅ shipped (inline `file-changed` diff
   cards in the timeline; `session_load` JSONL replayed into chat + model
   context on workspace open).
8. **Checkpoints/undo UI + cost/token ledger** — ✅ shipped (StatusBar
   CheckpointMenu: save / list / one-click revert + auto-checkpoint on plan
   approve; per-session `Σ tokens · tools · time` ledger in the StatusBar).
9. **P0 trust & turn quality** — ✅ shipped: turn lifecycle `outcome` + badges
   (P0-1); token accounting breakdown incl. cache/reasoning (P0-2); remote
   stall watchdog + retry/backoff (P0-3); `.ai/audit.jsonl` tool-verdict audit
   log + AuditMenu panel (P0-4); permission decision memory
   `allow_once / allow_session / always_allow` (P0-5).
10. **Next: smoke test + remaining P1 items** — run an agentic task and verify
    outcome badges, per-turn token breakdown, AuditMenu entries, "allow for
    session" not re-prompting, and `always_allow` writing `.ai/policy.json`;
    kill the network mid-stream → stall abort; verify `/decompose` parallel
    subtask chips + grouped step timeline and `/plan → Approve` phases.
    **P1-6 shipped**: persistent `create_plan`/`execute_plan` with per-item
    focused loops, `read_plan`, `update_plan`, `.ai/plan.json` + `.ai/plan.md`
    persistence, plan-step events in the timeline. **P1-7 shipped**:
    `set_todo_list` / `get_todo_list` / `mark_todo_item_done` persisted to
    `.ai/todos.json`, live `TodoCard` via `agent://todo-update`, and the
    orchestrator refuses to end a session while items remain open (bounded
    nudges). Bionic BN-2…BN-8 also shipped (see PROJECT_STATUS). Remaining P1:
    first-class subagents, `ask_question`/`send_to_user`, git blame/push/pull/
    branches/PR/CI, file delete/read_lints, `ast-grep`-style tree-sitter
    queries, token-budget-aware tool fan-out caps, KV-cache reuse across steps,
    model hot-swap.
