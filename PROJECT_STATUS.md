# Project Status — AI Editor

_Last updated: 2026-08-14 (skills & rules → model pass — IN PROGRESS). This file
is the source of truth for the session's progress. Read it at session start;
update it whenever milestones change. Strategic plan: see `ROADMAP.md`._

## ⏳ IN PROGRESS — "read any available skills & rules, train the model to work per them"

Goal from the user: the agent app should automatically read every available
skill and rule and behave according to them, instead of only the manually
toggled ones. Verification (`cargo check` / `cargo test` / `tsc` / `npm run
build`) is running — see "Verify:" line below once done.

### What changed this pass (2026-08-14, skills & rules pass)

**Gap found:** skills were *opt-in* (`parse_skill` set `active: false`) so the
model never saw any skill unless the user toggled it in the KnowledgePanel;
the model had no way to load a skill on demand; `prompt.ts` never told the
model that the rules/skills sections in context are binding.

**1. Skills are now auto-active (opt-out) — `skills.rs`**
- `parse_skill` defaults `active: true`, so every available skill under
  `{workspace}/.ai/skills/` (and the user-global `{config_dir}/skills/`) is
  read and pinned into the context automatically. User toggles are preserved
  across rescans and can turn any skill off from the KnowledgePanel.
- `active_skills_content` gained context-budget protection: per-skill body cap
  (`SKILL_BODY_CAP` = 3000 chars) + total cap (`SKILL_TOTAL_CAP` = 24000 chars),
  skills sorted by name, and a footer listing clipped skills with a pointer to
  the new `read_skill` tool — so skills can never blow the KV cache.
- New `get_skill(name)` + `skill_names()` accessors.

**2. New `read_skill` tool — model can load any skill's full text on demand**
- `ToolCall::ReadSkill { name }` (serde tag parse), schema registered in
  `core.rs`, dispatched in `tools.rs`, read-only so it defaults to **allow** in
  `policy.rs` `default_allow`.
- Returns the complete, untruncated skill body (name/description/source) so the
  model can apply a clipped skill end-to-end; unknown name → helpful error
  listing all available skills.
- Tool inventory: 15 → **16** (add `read_skill`).

**3. Shared knowledge state — `main.rs` + `mod.rs`**
- `ToolState` gained `knowledge: Arc<KnowledgeState>`; `run()` now creates ONE
  `Arc<KnowledgeState>` managed by Tauri and handed into `ToolState`, so a scan
  (`knowledge_scan` / `agent_set_workspace`) is immediately visible to both the
  UI commands and the agent's `read_skill` tool. All knowledge commands now take
  `State<'_, Arc<KnowledgeState>>`.

**4. System prompt now makes rules & skills binding — `prompt.ts`**
- New "Knowledge" rules: the `## Project rules` section (AGENTS.md/.cursorrules/
  CLAUDE.md/.ai/rules) is binding for every task; `## Skill instructions` are
  the available skills and must be applied when they match the task; a skill
  clipped from context must be loaded in full via `read_skill` before use; do
  not invent skills that don't exist.
- `read_skill` added to the documented tool list.

**5. Frontend wiring — `App.tsx` + `KnowledgePanel.tsx`**
- `selectWorkspace` now calls `knowledgeScan()` (not just `knowledgeReport()`) so
  rules+skills are scanned and pinned the moment a workspace opens.
- `KnowledgePanel` copy updated: skills are auto-active (opt-out) with per-skill
  toggles; `✧ N skills` StatusBar chip now reflects the auto-activated set.

### Changed files (this pass, so far)
- `src-tauri/src/agent/skills.rs` — auto-active default, caps, `get_skill`/
  `skill_names`, module doc, 2 new tests
- `src-tauri/src/agent/mod.rs` — `ToolCall::ReadSkill` (+name/summary),
  `ToolState.knowledge: Arc<KnowledgeState>`
- `src-tauri/src/agent/core.rs` — `read_skill` JSON schema
- `src-tauri/src/agent/tools.rs` — dispatch arm + `read_skill` impl
- `src-tauri/src/agent/policy.rs` — `read_skill` in `default_allow`
- `src-tauri/src/main.rs` — shared `Arc<KnowledgeState>` + `State` types
- `src/lib/prompt.ts` — binding rules/skills instructions + `read_skill` doc
- `src/App.tsx` — `knowledgeScan` on workspace select
- `src/components/KnowledgePanel.tsx` — opt-out copy

### Verify: (pending — `cargo check` / `cargo test` / `tsc` / `npm run build`)
### Next step
Finish verification, then smoke test `npm run tauri:dev`: open a workspace with
`.ai/skills/*.md`, confirm the model sees `## Skill instructions` in context and
can call `read_skill`; then continue the roadmap (diff preview UI → session
resume → semantic search → parallel agent threads).

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
code editor.

- Frontend: React 19 + Vite 6 + Tailwind v4 + Monaco editor
- Desktop shell: Tauri 2 (Rust) — frameless window, host-side fs + model I/O
- Local inference: `llama-cpp-2` (Rust bindings to llama.cpp) — CPU-only build,
  GGUF models
- Streaming: Rust worker thread → bounded crossbeam MPSC → Tauri events →
  rAF-batched React state

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
- Next: `npm run tauri dev` → smoke-test: load a GGUF, open a workspace folder,
  edit a file, send a chat prompt, verify streaming + tok/s stats, Stop button.
- Known follow-ups when testing: ~~acquire a GGUF model~~ **done** —
  `models\qwen2.5-0.5b-instruct-q4_k_m.gguf`; verify model `info()` metadata,
  KV-cache reuse, stop-word trimming.

## Pending / next steps (ordered)

1. `npm run tauri dev` (compiles the dev binary + links ~1-3 min, then opens the
   window). Command: `npm run tauri:dev` from `D:\ai`.
2. Smoke-test the full flow (load model → chat → edit → save).
3. `npm run tauri build` later for a production bundle (release build is slow;
   uses opt-level 3 + lto).

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
└─ src-tauri/
   ├─ Cargo.toml, build.rs, tauri.conf.json, capabilities/default.json, icons/
   └─ src/ engine.rs, main.rs
```

## Session environment gotchas

- Shell is **pwsh 7 on win32**; workdir `D:\ai`
- cargo/rustc need `$env:Path` prepend every new shell
- CMake only inside VS dir (add to PATH for cargo)
- `npm run build` runs `tsc && vite build` (typecheck gate)
- Installers that need UAC may be silently canceled in this session — prefer
  approaches that run detached/quiet and poll for artifacts
