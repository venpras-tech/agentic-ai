# Chat Intent / Execution Fix — Todo List

Status: implementation and automated verification complete. Live-model QA and follow-ups pending.

## Context

Chat was forced into an execution cycle by two mechanisms:

1. The agent system prompt declared the model an "Autonomous Execution Engine … not a conversational assistant" and required immediate tool calls (`ui/src/lib/agentPrompt.ts`).
2. The Rust harness re-prompted any text-only reply on a keyword-matched "coding" message with "You MUST use your tools … Start working NOW" (`src-tauri/src/agent/orchestrator.rs`).

The fix makes intent model-decided, matching Cursor (Agent/Ask), Claude Code (normal/plan), and ChatGPT-style tool use: plain text is a valid final answer; the harness enforces permissions, not tool usage.

## Done

- [x] Replace execution-only agent prompt — `ui/src/lib/agentPrompt.ts`
- [x] Align small-model prompt — `ui/src/lib/agentPrepare.ts` (`COMPACT_AGENT_SYSTEM_PROMPT`)
- [x] Align Ask mode — `ui/src/lib/agentPrepare.ts` (`ASK_MODE_APPEND`)
- [x] Align backend fallback prompt — `src-tauri/src/agent/prompt.rs` (`DEFAULT_AGENT_SYSTEM`)
- [x] Remove harness tool-use enforcement — `src-tauri/src/agent/orchestrator.rs`
  - Deleted `MAX_TOOL_USE_ENFORCEMENT` and the "MUST use your tools / Start working NOW" injection.
- [x] Remove refusal → tool coercion — `src-tauri/src/agent/orchestrator.rs`
- [x] Make text-only responses terminate the turn — `src-tauri/src/agent/orchestrator.rs`
- [x] Stop hidden memory extraction on chat turns — `maybe_extract_memory` now requires at least one `role:"tool"` message
- [x] Soften repeat-guard instruction (no longer pushes `<execute_tool>`)
- [x] Add UI regression tests — `ui/src/lib/agentPrompt.test.ts` (5 tests)
- [x] Add Rust prompt tests — `src-tauri/src/agent/prompt.rs`
- [x] Fix new-chat display bug (user message + reply not shown) — `ui/src/hooks/useAgentStream.ts`
  - Stream events are tagged with a `session_id`; the UI subscription applied every
    event to whatever chat was on screen. Handlers are now session-scoped
    (`isActiveSession`, `claimNextSession`, `setActiveSessionId`,
    `resetActiveSession`), and `AppContext` clears the active session on
    new-chat / conversation switch, so a run started in one chat can no longer
    bleed into another.
- [x] Add regression tests for session-scoped streaming — `ui/src/hooks/useAgentStream.test.ts` (15 tests)
- [x] Run verification:
  - [x] `ui: npx tsc --noEmit` — PASS
  - [x] `ui: npm test` — PASS 192/192 (18 files)
  - [x] `ui: npm run build` — PASS
  - [x] `cargo test --bins` — PASS 284 passed, 4 ignored
  - [x] `rustfmt --check` on changed Rust files — PASS

## Context size + llama.cpp settings (new)

- [x] Raise the usable context without touching chat behavior: context is a
  *model-load* parameter, so the LLM page exposes **Context length** with an
  "Apply & reload model" button (`ui/src/context/AppContext.tsx` `reloadModel`)
  and a hint showing the KV-cache cost.
- [x] Live KV-cache estimate — `ui/src/lib/llamaCpp.ts` (`estimateKvBytes`, `formatBytes`)
- [x] New **llama.cpp** tab in Settings (`ui/src/pages/SettingsPage.tsx`)
  - Build/backend snapshot (bindings version, device count, `mmap`/`mlock`/GPU features)
  - Loaded-engine report (workers, resolved `ModelInitParams`, quantized-cache types)
  - Editable load-parameter form (batch/ubatch, workers, RoPE, cache types, toggles, raw args)
  - Change summary + "Save" / "Save & reload" so a `contextSize` change takes effect immediately
- [x] Backend inspect command — `src-tauri/src/commands/model.rs` (`llama_runtime_info`),
  `src-tauri/src/engine.rs` (`llama_build_info`/`LlamaBuildInfo`), `InferenceState`
  records the resolved `ModelInitParams` at load time (`src-tauri/src/main.rs`).
- [x] Persist advanced llama.cpp fields — `ui/src/lib/localModelConfig.ts`
  (`LOCAL_CONFIG_KEYS` extended; type-aware `sanitizeLocalConfig`; `applyClearedConfig`
  so a cleared field is truly removed).
- [x] Helpers + tests — `ui/src/lib/llamaCpp.ts` / `llamaCpp.test.ts` (13 tests),
  `localModelConfig.test.ts` extended.

## Slow-response diagnosis + hardening (new)

Diagnosed from `%APPDATA%\dev.local.ai-editor\logs` on the field machine
(i5-11300H, 4c/8t, 7.8 GB RAM, no GPU, `nGpuLayers: 0`). The slow replies are a
**memory** problem, not a code bug — measured ~260 MB free RAM and
7,000–13,600 pages/sec while a 5 GiB model was resident, so the OS thrashes to
disk (first token took 229 s). The loaded `model.gguf` is an LFM2 2.70B in
**F16** (5.02 GiB, 16.00 BPW) — as heavy as a 7B Q5 for a third of the quality.

- [x] **Prune stale model configs** — `src-tauri/src/commands/settings.rs`
  - `localModelConfigs` is keyed by GGUF path and was never validated against
    disk, so a deleted / never-finished download kept showing in Settings.
  - `prune_missing_model_configs` drops path-keyed entries whose `.gguf` is
    missing, always keeps the `"default"` key, and leaves non-`.gguf` keys
    untouched; `local_model_configs_load` prunes, logs, and rewrites the file.
  - Test: `prune_drops_only_missing_gguf_paths`.
- [x] **RAM-fit warning (weights, not just KV cache)** — `ui/src/lib/modelSuggest.ts`
  - `suggestLlmParams` now returns `fitsRamWithWeights` and `totalGiB`
    (weights + KV cache). The old `fitsRam` only compared the KV cache, so a
    5 GiB model on a 7.8 GB box reported "fine".
  - New note flags an **unquantised F16/BF16** build (> 12 bits/weight) as
    wastefully large, with the "RAM needed" figure surfaced in
    Settings → LLM (`ui/src/pages/SettingsPage.tsx`).
  - Tests: `ui/src/lib/modelSuggest.test.ts` (5 tests).
- [x] **Harden the decode path** — `src-tauri/src/engine.rs`
  - `Failed to decode token: Unknown Token Type` (llama.cpp) aborted the whole
    agent step via `?`. A degraded / hybrid-arch GGUF can sample an undefined
    vocab entry, so the token is now logged (`crate::logging::warn`) and
    **skipped** (KV advanced, generation continues) exactly like an empty piece.
- [x] Re-run verification:
  - [x] `cargo test --bins` — PASS 284 passed, 4 ignored
  - [x] `npm test` — PASS 192/192 (18 files)
  - [x] `ui: npx tsc --noEmit` — PASS
  - [x] `rustfmt --check` on `engine.rs` + `commands/settings.rs` — PASS

### Notes for the user

- **"Agent step 2 failed" is not a bug.** `orchestrator.rs` runs one full
  generation per step. Step 1 returned text-only (`0 tool call(s)`), which
  normally ends the turn — but the **TODO-nudge** block (`MAX_TODO_NUDGES = 2`)
  bounced it back because the session had open todo items. That nudge is
  deliberate ("a session cannot finish while items remain") and was left as is;
  step 2 is what then hit the decode error above.
- **The real throughput fix is a smaller quant**, not a code change: re-download
  the 2.7B at Q4/Q5 (~1.6–2.1 GiB) or a 3–4B at Q4 (~2 GiB). A 0.5B/1.5B model
  is below the app's own `SMALL_MODEL_PARAM_CEILING` (2B) and would loop.

## Remaining

- [ ] **Live-model manual QA**
  - Load a real GGUF or remote model and verify:
    - `hello` → no tools
    - `what does this function do?` → answer, no edits
    - `fix the login bug` → tools
    - Ask mode → read-only
    - Plan mode → plan only
  - New chat → send `hi` → the user bubble **and** the streamed reply both appear.
  - Switch chats mid-run → no output bleeds into the other chat.
  - Settings → LLM → raise **Context length** → "Apply & reload model" → the
    context meter denominator increases and the model reports the new size.
  - Settings → LLM → with a model whose weights + KV cache exceed free RAM
    (e.g. the F16 `model.gguf`), the strong **RAM-needed** warning shows
    (~5.0 GiB needed vs ~0.3 GiB free).
  - Settings → LLM → a stale `localModelConfigs` entry (path missing on disk)
    disappears after the settings load, and a log line records the prune.
  - Settings → llama.cpp → build/engine info renders; a form change + "Save & reload"
    persists and is reflected in the resolved-parameters report.

- [ ] **Add an orchestrator regression test**
  - Inject a fake `TextGenerator` returning text-only; assert the loop stops after one step with no tool dispatch.

- [ ] **Folder-less chat fast path**
  - `prepareAgentTurn` currently requires a project folder even for a greeting.
  - Allow plain chat before opening a workspace, like Cursor/Claude.

- [ ] **Wire the dead `agent://step` / `agent://aborted` events**
  - `ui/src/lib/ipc.ts:736–752` subscribes to events the backend never emits.
  - Needed for accurate thinking/step/aborted UI feedback.

- [ ] **Consider a dedicated "Chat" mode**
  - Optional fourth mode that always uses `streamInference` (no tools), leaving Agent for workspace actions.

- [ ] **Update docs**
  - `docs/LLM_INTEGRATION.md` — tool count and mode semantics
  - `docs/manual-test-script.md` — new intent-routing cases and current test counts
  - `PROJECT_STATUS.md` — remove stale `[x]` claims

- [ ] **Re-run the audit**
  - `D:\ai\audit-report.html` predates these changes; regenerate after QA.

## Acceptance criteria

- A greeting or general question in Agent mode produces a plain answer with zero tool calls and no execution graph.
- A workspace action request still produces tool calls and results.
- Ask mode never mutates files; Plan mode never calls tools.
- No code path re-prompts the model solely because it answered in text.
- Test suites remain green.
