# LLM Integration — Request Pipeline & Tool-Call Protocol

_Audience: developers maintaining the AI Editor's model integration._
_Last updated: 2026-08-24. Source of truth for behaviour; `PROJECT_STATUS.md`
tracks when things changed._

---

## 1. Architecture at a glance

```
┌────────────── React frontend (src/) ──────────────┐
│ App.tsx        builds request payloads (GenParams)│
│   │  api.streamInference / api.agentRunTask       │
│   ▼  Tauri IPC (invoke)          ◄── events ──┐   │
└──────────────────────────────────────────────┼───┘
┌────────────── Rust host (src-tauri/) ─────────┼───┐
│ main.rs commands                              │   │
│  ├─ stream_inference   (plain chat)           │   │
│  └─ agent_run_task      (agentic loop)         │   │
│   │                                           │   │
│   ├─ ContextManager (agent/context.rs)        │   │
│   │    system → pinned buffers → history      │   │
│   ├─ orchestrator.rs  build_prompt/chat_turns │   │
│   ├─ engine.rs  chat template · sampler · KV  │   │
│   │    llama-cpp-2 (local GGUF)               │   │
│   │    remote.rs (OpenAI/Anthropic providers) │   │
│   ├─ core.rs    parse_tool_calls              │   │
│   ├─ policy.rs  allow / ask / deny gate       │   │
│   └─ tools.rs   50 tools + dispatch           │   │
│   │                                           │   │
│   └─ spawn_emitter: WorkerEvent → app.emit ───┘   │
│        inference-token / -done / -error,          │
│        agent://tool-event, agent://file-changed…  │
└───────────────────────────────────────────────────┘
```

Two entry points share one engine pool but different prompt pipelines:

| | Plain chat (`stream_inference`) | Agent (`agent_run_task`) |
|---|---|---|
| History | ContextManager snapshot (multi-turn since 2026-08-24; previously single-shot) | ContextManager snapshot **+** per-step working history |
| System prompt | Pinned system prompt from the context manager, if set | Full agent system prompt (`prompt.ts` catalog compiled into `core.rs` schemas) |
| Tool calls | Never — raw completion | Parsed every step, executed, fed back |
| Loop | One generate | generate → tools → feedback × ≤6 steps |

---

## 2. How the request is formatted

### 2.1 Structured turns → chat template (preferred path)

Every generation request carries an optional `messages: Vec<ChatTurn>`
(`engine.rs`). When present **and** the loaded GGUF embeds a chat template,
`render_chat_template` (`engine.rs`) renders them via:

```rust
let template = engine.model.chat_template(None)?;            // baked-in Jinja
engine.model.apply_chat_template(&template, &chat, true)?;   // add_ass = true
```

* Role mapping — templates only know `system`/`user`/`assistant`, so our
  extended roles map onto `system` with their section headers preserved inside
  the content: `system`, `context`, `rules`, `skill`, `plan`, `tool` → `system`;
  `user`/`assistant` pass through.
* Empty messages are dropped; `add_ass = true` leaves the assistant tag open
  so the completion continues it.
* **Fallback chain** (any failure ⇒ next): messages empty/unparsable → model
  has no template metadata (base models) → flat `request.prompt`.

This was the root fix for "the model ignores the app": a ChatML-tuned model was
receiving bare `## User …` text and answered with degenerate loops. Using each
model's own template keeps the app in sync across Llama-3 / Mistral / Qwen /
ChatML families without code changes.

### 2.2 Flat fallback format (`build_prompt`, orchestrator.rs)

Template-less models get one plain-text completion string:

```
## System instructions
{system content}

## Active file contents        ← role "context" (pinned file buffer)
## Project rules               ← role "rules" (.ai/rules.md)
## Skill instructions          ← role "skill" (@-mentioned skill)
## Approved plan               ← role "plan"
## User                        ← history user turns
## Assistant                   ← history assistant turns
## Tool result                 ← tool feedback (see §4)
## User                        ← current turn (deduped if already last)
[## Current subtask …]         ← decompose mode only
```

Roles are mapped in `build_prompt`; unknown roles are skipped.

### 2.3 Conversation memory

The frontend pushes each finished turn into the shared `ContextManager`
(`agent/context.rs`) *before* invoking:

```ts
await api.contextPushTurn("user", trimmed);   // then invoke…
// onDone:
api.contextPushTurn("assistant", text);
```

`ContextManager` enforces the token budget: pinned entries survive, oldest
evictable turns drop first, and the system prompt is trimmed only as a last
resort. Both entry points read `.messages()` at invoke time; `stream_inference`
and `build_prompt` both dedupe the just-pushed trailing user turn so it is
never sent twice.

---

## 3. Sampling parameters

Chain order mirrors llama.cpp's own defaults (`build_sampler`, engine.rs):

1. **Repetition penalty** — `penalties(last_n=64, rp, 0, 0)` when rp > 1.0.
   Default **1.15** (`REPEAT_PENALTY_DEFAULT`), clamped 1.0–2.0, wired
   end-to-end (UI "repeat" field → GenParams.repeatPenalty → every request).
2. **Temperature** — default 0.8, clamp 0–4.
3. **Top-p** — default 0.95, clamp 0–1.
4. **dist(seed)** or **greedy** when temperature = 0 (deterministic).

Stop handling: stop words checked against the rolling output suffix
(`has_stop_suffix`) and never emitted; EOG tokens end generation natively.
Agent requests default to `["<|endoftext|>"]` only — never truncate mid-tool-call;
plain chat adds `"\n\n---"` and `"User:"`.

---

## 4. Tool calls (agentic loop)

### 4.1 Wire format the model must emit

Inside its normal response text, the model emits fenced JSON blocks tagged
with custom markers (defined in the system prompt, `src/lib/prompt.ts`):

```markdown
I'll check the file first.

<execute_tool>
{"type":"read_file","path":"src/main.rs"}
</execute_tool>
```

* `parse_tool_calls` (`agent/core.rs`) scans for `<execute_tool>…</execute_tool>`,
  strips an optional ```json fence, and deserializes into the `ToolCall` enum
  (tagged by `type`, ~50 variants).
* Malformed payloads are skipped with a warning; unclosed tags abort the scan.
* Multiple calls per step execute **in parallel** (subagents permitting).

### 4.2 Policy gate (policy.rs)

Each call passes through `policy::check()` before execution:

| Verdict | Behaviour |
|---|---|
| `allow` (default-allow list / remembered decisions / YOLO routine cmds) | runs immediately |
| `ask` | emits `agent://permission-request`, blocks on a oneshot channel (120 s timeout ⇒ deny); frontend shows PermissionModal |
| `deny` (red zone) | refused with explanation, no execution |

Decisions can be remembered (`always_allow`) and are audited to
`.ai/audit.jsonl`. Subagents run under restricted profiles: `CHILD_NEVER`
tools (e.g. `ask_question`, `send_to_user`) are hard-refused for children.

### 4.3 Execution & feedback

`tools::dispatch()` runs the call (async, interrupt-aware, per-tool timeouts)
and returns `ToolResult { success, summary, stdout?, error?, duration_ms }`.
Live progress streams as `agent://tool-event`
(running → done/error, with `session_id` pinning the event to the right UI
turn). The result is formatted by `format_tool_feedback`:

```
`read_file` succeeded in 12ms: Read src/main.rs (210 lines)
{truncated stdout, TOOL_FEEDBACK_LIMIT chars}
```

pushed into the working history as role `tool`, and the loop generates again —
the model sees its own prior assistant turns plus these results. After the
step budget: decomposed subtasks may fan out (orchestrator), and a final
`run_summary` pass writes the plain-text report. Blocking questions use
`ask_question` → `agent://question-request` → inline QuestionCard →
`agent_respond_question(requestId, answer)`.

---

## 5. Response processing & events

Generation streams token pieces over a crossbeam channel; `spawn_emitter`
(main.rs) translates `WorkerEvent`s into Tauri events the frontend subscribes
to once (`useEngineEvents`):

| Event | Payload | UI consumer |
|---|---|---|
| `inference-started` | `{sessionId}` | creates the anchor message |
| `inference-token` | `{sessionId, delta}` | RAF-batched stream buffer |
| `inference-done` | `{sessionId, done: InferenceDone}` | finalizes turn, stats line, ledger |
| `inference-error` | `{sessionId, message}` | error bubble |
| `execution-aborted` | reason payload | error banner |
| `agent://tool-event`, `-tool-output`, `-file-changed`, `-plan-step`, `-todo-update`, `agent-step`, `agent-subtask`, permission/question requests | … | inline agentic feed |

`InferenceDone` always arrives — including cancellations
(`outcome: "interrupted"`, real token counts), which is why interrupted turns
still show badge + stats + copy.

Post-processing per turn: content copied from the stream buffer into the
message, persisted via `sessionAppend`, pushed back into the ContextManager,
and (plan mode) held for explicit Approve before any execution.

---

## 6. Error handling & robustness

* **Cancellation**: one circuit-breaker token per run (`InterruptState.arm()`);
  polled between decode steps and between tool steps; tool subprocesses die
  with it. A fresh arm prevents stale cancels leaking into the next run.
* **Remote stall detection** (remote.rs, P0-3): no-token watchdog with
  retry/backoff; local generation surfaces errors as `inference-error`.
* **Empty-output guard**: completed-but-empty generations get a synthetic
  hint token instead of a silent hang (main.rs).
* **Per-tool timeouts** (e.g. `GIT_TIMEOUT` 60 s) and bounded feedback size
  (`TOOL_FEEDBACK_LIMIT`) keep the context clean.
* **Degenerate repetition**: repeat penalty (§3) breaks loops; if a base
  (non-instruct) GGUF still produces garbage, load an instruct-tuned model —
  template rendering cannot help models with no template metadata.

---

## 7. Troubleshooting sync issues

| Symptom | Likely cause | Fix |
|---|---|---|
| Model echoes prompt-like scaffolding / loops forever | No repeat penalty (fixed 2026-08-24) or base model | Keep repeat ≥ 1.1; use instruct GGUF with embedded template |
| Responses ignore conversation history | Older build: plain chat was single-shot | Now multi-turn via ContextManager; verify `contextPushTurn` fires |
| Wrong format / refuses tools after switching models | Template mismatch | Ensure GGUF has `tokenizer.chat_template` metadata; fallback uses flat format otherwise |
| Tool calls never execute | Model not emitting `<execute_tool>` JSON, or policy denied silently | Check audit log `.ai/audit.jsonl`; verify system prompt version matches `core.rs::tool_schemas()` |
| Events land on the wrong turn | Missing `session_id` on tool events | All emitters now stamp `session_id`; frontend falls back to active session |

Change checklist when touching this pipeline:
1. New tool? `mod.rs` variant + name/summary, `core.rs` schema + tests,
   `prompt.ts` doc entry, dispatch arm, policy defaults, subagent lists.
2. Prompt/sampling change? Bump `PROJECT_STATUS.md`, re-run gates:
   `cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test`
   and `npx tsc --noEmit && npm test && npm run build`.
