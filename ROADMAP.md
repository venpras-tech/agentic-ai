# Roadmap — From "local chat + editor" to "super agentic coding agent"

_Last updated: 2026-08-14 (super-agentic pass). Strategic plan derived from a
full codebase audit (`src/` + `src-tauri/src/`). Companion to
`PROJECT_STATUS.md` (session status / source of truth for what has shipped)._
_Tick marks (`[x]`) = shipped; `[ ]` = remaining._

## Where we are today

Solid engineering skeleton: streaming llama.cpp + OpenAI-compatible remote
engines, an agent orchestrator (generate → parse → dispatch → feedback, plan
mode, parallel `join_all` dispatch, self-healing retry), a 15-tool layer
(glob, content-search, AST, read, diff-apply, write, streaming terminal, MCP,
tests, git ×5, create-skill), a circuit breaker, a context eviction engine,
permissions/policy, skills & rules, editor sync, Plan→Act UI, slash commands,
and a clean Tauri 2 / React 19 shell.

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
   (`.ai/skills`), session resume. [x] partial — history + instructions + skills
   shipped; **session resume (replay JSONL on workspace open) pending**.
4. **Git-native workflow** — `git status` / `git diff` tools, checkpoint before
   edits, commit, revert. [x] shipped (5 git tools) — auto-checkpoint/undo
   buttons in UI pending.
5. **Test-driven feedback loop** — auto-run tests/typecheck after edits and
   self-correct on failures. [x] shipped (`run_tests`, verify flag, `auto-verify`
   nudge after edits).
6. **Self-healing retry** — on tool failure, feed the error back and retry with
   a bounded budget instead of dying. [x] shipped (SELF-ASSESSMENT injection,
   `stuck` stop-reason after 3 consecutive all-failed steps).
7. **Parallel tool execution** — parallel read-only fan-out cuts latency. [x]
   shipped (`join_all`) — needs a real-model latency validation run.
8. **Semantic codebase search** — embeddings index + ranking (glob + AST is not
   "find where the auth logic is"). [ ] pending — content regex search shipped
   as an interim; embeddings next.
9. **File-write preview & editor sync** — diffs are applied to disk but the open
   Monaco editor never refreshes; need diff preview + reload. [x] editor reload
   shipped (`agent://file-changed`) — diff *preview* UI still pending.
10. **Live tool output streaming** — terminal stdout arrives only on completion.
    [x] shipped (`agent://tool-output`, line streaming).
11. **Token / cost & step telemetry** — per-step tokens, tool durations, context
    pressure. [x] partial — `agent-step` events + tool durations render;
    aggregate cost/token ledger UI pending.

## Tier 3 — Product polish

- Agent **timeline UI**: collapsible step/tool cards with status, duration,
  expandable stdout. [x] tool cards + live stdout + step chip; full step-group
  timeline UI pending.
- **Checkpoints / snapshots** + one-click revert. [ ] pending (backend tools
  exist; UI buttons pending).
- **Slash commands + command palette** (`/fix`, `/test`, `/commit`), plan/act
  toggle. [x] shipped (`/plan /act /fix /test /commit /skills /clear`).
- Model **settings UI** persisted. [x] shipped (gen params + remote prefill via
  `settings_load`/`save`).
- **Sub-task decomposition** (`/decompose`) — plan → per-subtask focused loops →
  summary. [x] shipped sequential; **parallel agent threads** (multiple engine
  instances) still pending — blocked by single-engine architecture.

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

**Critical constraint**: `engine.rs` transmutes a `&'static mut` engine onto a
worker thread per generation. The orchestrator loop makes this central. Target
one long-lived engine thread owning `LlamaContext` (no transmute across the
loop) communicating via channels — removes unsoundness risk *and* enables
KV-cache reuse across steps (biggest multi-turn speedup).

## Implementation order (highest ROI first)

1. **Agent loop + context feeding + session-id fix** (Tier 1.1–1.3) — ✅ shipped
2. **Model router** (Tier 1.4) — ✅ shipped
3. **Permission/safety + plan mode** (Tier 2.1–2.2) — ✅ shipped
4. **Git checkpoints + retry + tests loop** (Tier 2.4–2.6) — ✅ shipped
5. **Semantic search + parallel tools + timeline UI** (Tier 2.7–2.11) — ✅
   parallel tools + timeline + editor sync + live streaming shipped; **semantic
   embeddings search still pending**.
6. **Sub-task decomposition** (Tier 3) — ✅ shipped sequential (`/decompose`:
   model emits a JSON subtask list; each subtask runs a focused agent loop with
   its own step budget; a final plain-text summary closes the task; failing
   subtasks are recorded and the rest continue). **Parallel agent threads**
   (multiple model instances) still pending.
7. **Next: diff preview UI** — render `agent://file-changed` diffs inline.
8. **Next: session resume** — replay `session_load` JSONL into the chat on
   workspace open (persistent task memory).
