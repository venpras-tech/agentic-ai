# Project Status — AI Editor

_Last updated: 2026-08-23 (**agentic chat fixes shipped** — model auto-load on
startup, small-talk hijack fix, `calculate` tool, UI/BE/LLM console tags; all
gates green). This file is the source of truth for the session's progress.
Read it at session start; update it whenever milestones change. Strategic
plan: see `ROADMAP.md`._

---

## ✅ SHIPPED — Unified console pipeline: in-app Console window + rolling file appender

- `logging.rs`: `init(dir, sink)` called from the Tauri setup hook. Every
  `[BE]`/`[LLM]` line now goes to **stderr + rolling file + webview**.
  File format: `{app-data}/logs/ai_editor_{ddMMyyyy}_{HHmmssSSS}.{ZZZZ}.log`
  with a 100 MB size-based roller (`.0001` → `.0002` …); unwritable dir falls
  back to stderr-only.
- Webview mirror: each line is emitted as a `console-log` Tauri event;
  new `src/lib/consoleBus.ts` funnels backend lines (`parseBackendLine`) and
  frontend lines (`uiLog` now publishes too) into App state rendered by the
  existing ConsolePanel (800-entry ring cap). Filter box matches on tag/phase.

### Verification
- clippy zero warnings · cargo test 76 passed · fmt ✓ · tsc ✓ · build ✓ ·
  14 vitest ✓

---

## ✅ SHIPPED — Agentic chat diagnosis + fixes (greeting path, auto-load, calculate tool)

### Diagnosis
1. **Root cause of "chat does nothing, even to Hello":** no model was loaded
   at startup and nothing restored one. Every prompt hit the
   `ok_or("No model loaded")` guard before any event reached the UI.
2. **Small-talk interceptors were too greedy:** both `main.rs` (plain chat)
   and `agent/orchestrator.rs` (agent loop) matched ANY ≤ 2-word prompt under
   ~20 chars, so real queries like `2+3` or `Calculate 5+7` got a canned
   "Hi there!" instead of model/tool execution.

### Fixes (2026-08-23)
- **Model auto-load:** new `auto_load_model` command (`main.rs`) resolves a
  model without user interaction: saved `settings.json:modelPath` → HF-
  downloaded models → `./models/*.gguf` next to the working dir. Successful
  loads persist their path via `persist_model_path` inside
  `install_local_model`. Frontend calls it once when `model_status` is empty
  (`App.tsx`), then pushes `AGENT_SYSTEM_PROMPT`.
- **Shared small-talk module** `src-tauri/src/agent/smalltalk.rs`: vocabulary-
  gated detection (lead word must be hi/hello/hey/thanks/bye/… or exact
  phrase) with length caps; both call sites now delegate to it; duplicate
  logic removed from orchestrator.
- **`calculate` tool** for deterministic math ("What is 2+3?"): new
  `ToolCall::Calculate` variant + pure recursive-descent evaluator
  (`eval_arithmetic`: + - * / % ^ parens unary-minus pi/e; rejects division
  by zero / unknown identifiers), schema registered in `core.rs`; policy
  allows it as a non-path ROUTINE tool by default.
- **Console source tags:** Rust log lines now carry `[LLM]` / `[BE]` tags
  (`logging.rs`); webview logs emit `[UI]` via new dev-only `src/lib/uiLog.ts`,
  wired into StatusIndicator phase transitions.

### Changed files
- NEW `src-tauri/src/agent/smalltalk.rs`, NEW `src/lib/uiLog.ts`
- `src-tauri/src/main.rs` · `src-tauri/src/logging.rs` ·
  `src-tauri/src/agent/{mod,orchestrator,tools,core}.rs`
- `src/App.tsx` · `src/lib/ipc.ts` · `src/components/StatusIndicator.tsx`

### Verification
- `cargo fmt --check` ✓ · clippy zero warnings ✓ · **76 tests passed**
  (incl. smalltalk negatives "Calculate 5+7"/"2+3", arithmetic precedence,
  serde-tagged dispatch) · `npx tsc --noEmit` ✓ · `npm run build` ✓ ·
  14 vitest tests ✓
- Manual: `npm run tauri:dev` → model auto-loads → "Hello" gets canned reply
  instantly → agent-mode "Calculate 5+7" runs the real loop → terminal shows
  interleaved `[UI] [BE] [LLM]` lines.

---

## ✅ SHIPPED — LLM lifecycle console logs + animated chat status indicator

### What changed this pass (2026-08-23)

**1. Backend console logging (`src-tauri/src/logging.rs`, new)**
- Timestamped structured logger to stderr: `[YYYY-MM-DD HH:MM:SS.mmm] [LEVEL]
  [sess N] [  phase] message` (UTC, Hinnant civil-from-days; no new deps).
- Phases: `llm.request` (command entry: char counts + gen params, opt-in
  80-char prompt preview), `llm.stream` (first-token latency then ≥2 s /
  ≥512-char throttled progress via `StreamProgress`), `llm.step`,
  `llm.subtask`, `llm.done` (outcome, elapsed, all token counters),
  `llm.error`; tool lifecycle mirrored from `tools.rs::emit` (`▶ / ✓ / ✖`)
  plus `tool.permission` request/decision lines in `ask_approval`.
- Privacy: prompts are never logged unless `AI_EDITOR_LOG_PROMPTS=1`;
  errors truncated to 300 chars. 6 unit tests.

**2. Wiring (`main.rs`)**
- `spawn_emitter` now owns a per-session `HashMap<u64, StreamProgress>` and
  logs every `WorkerEvent` variant as it forwards to the webview.
- Entry-point logs added to `stream_inference` (+ greeting-shortcut note)
  and `agent_run_task`.

**3. Frontend status machine (`src/lib/chatStatus.ts`, new)**
- Pure reducer over the turn lifecycle:
  `idle → sending → thinking → streaming ⇄ working → complete | error`
  driven by a typed `ChatStatusEvent` union; stale hints (>45 s quiet,
  >10 s unacked submit) surface "still running — see Console".
- `StatusIndicator.tsx` (new): accessible (`role="status"`,
  `aria-live="polite"`, sr-only announcement) animated line between the
  timeline and composer — spinner while thinking/working, bouncing dots
  while streaming, green ✓ auto-hides after ~3.5 s, red ✕ on error.
- Wired in `App.tsx` (`useReducer` + dispatches from every engine event
  handler, submit/catch/reset) and rendered by `ChatPanel.tsx`
  (new required prop `status`).

**4. Tests**
- `src/lib/chatStatus.test.ts`: 14 vitest tests covering the transition
  graph, stale thresholds, session-id guards, and view derivation.
- `package.json`: `vitest` devDep + `"test": "vitest run"` script.

### Changed files (this pass)
- NEW `src-tauri/src/logging.rs` · NEW `src/lib/chatStatus.ts` ·
  NEW `src/components/StatusIndicator.tsx` · NEW `src/lib/chatStatus.test.ts`
- `src-tauri/src/main.rs` · `src-tauri/src/agent/tools.rs` ·
  `src/App.tsx` · `src/components/ChatPanel.tsx` · `package.json`

### Verification (this pass)
- `cargo fmt --check` clean · `cargo check` clean · `cargo clippy
  --all-targets` zero warnings · `cargo test` 70 passed / 1 ignored
- `npx tsc --noEmit` exit 0 · `npm run build` green · `npm test` 14 passed

---

## ✅ SHIPPED — Lekshan removal + stale-artifact purge, builds green

### What changed this pass (2026-08-23)

**1. Lekshan secondary-window feature fully excised**
- Audit found the source tree already clean (`MenuBar.tsx` menu item +
  `open_lekshan` invoke, `main.rs` command, `vite.config.ts` /
  `tsconfig.json` entries, `lekshan.html`, `src-tauri/capabilities/lekshan.json`
  all previously removed; `src-lekshan/` gone).
- `PROJECT_STATUS.md`: dropped the stale `src-lekshan/*` changed-files
  references from the boot-smoke pass log below.
- Stale artifact purge: `dist/` regenerated via a fresh production build
  (the previous `dist/assets/main-DJoGCe1M.js` bundle still embedded the
  "Open Lekshan…" menu entry); `src-tauri/gen/schemas/capabilities.json`
  confirmed to contain only the `default` capability.
- Remaining `target/` mentions are inert build-cache leftovers (old `.d` dep
  files / superseded build-dir `capabilities.json`) that cargo regenerates on
  rebuild and never reads from old hash dirs.

### Changed files (this pass)
- `PROJECT_STATUS.md` — this log + `src-lekshan` references scrubbed
- `dist/` — rebuilt from clean sources

### Verification (this pass)
- `npm run build` green (tsc exit 0 · vite build ✓)
- `cargo check` clean
- Grep sweep: no `lekshan` matches in `src/`, `src-tauri/src`,
  `src-tauri/gen`, `src-tauri/capabilities`, `dist/`, or root docs

---

## ✅ SHIPPED — boot-smoke `custom-protocol` fix, all builds & smokes green

### What changed this pass (2026-08-22, evening)

**1. Boot-smoke root cause FIXED (was failing since BN-2…BN-8 landed)**
- Symptom: release exe timed out (`AI_EDITOR_SMOKE_TIMEOUT`) — CDP probe showed
  the webview sitting on `chrome-error://chromewebdata` → `ERR_CONNECTION_REFUSED`
  against `http://localhost:1420`.
- Root cause: `[features]` never declared `custom-protocol`. Without it, tauri
  emits `cfg(dev)` even for `--release`, so binaries serve `devUrl` instead of
  embedding `../dist`. Plain `cargo build --release` (CI's recipe) could never
  pass.
- Fix: `Cargo.toml` gains `custom-protocol = ["tauri/custom-protocol"]`
  (NOT default — debug/dev keeps HMR); `ci.yml` boot-smoke now builds with
  `cargo build --release --features custom-protocol`.
- Diagnostics added permanently: `smoke_fail` command + early error reporter in
  `index.html` and `main.tsx` — webview-side boot failures now print
  `AI_EDITOR_SMOKE_FAIL: …` on stderr instead of silently timing out.
- **Verified**: `AI_EDITOR_SMOKE_OK`, exit 0 · live-GGUF headless chat ✅.

### Changed files (this pass)
- `src-tauri/Cargo.toml` — `custom-protocol` feature
- `.github/workflows/ci.yml` — boot-smoke builds with the feature
- `src-tauri/src/main.rs` — `smoke_fail` command (registered)
- `index.html` / `src/main.tsx` — boot-failure reporters
- Docs: `PROJECT_STATUS.md` / `ROADMAP.md`

### Verification (this pass)
- `npx tsc --noEmit` exit 0 · `npm run build` green
- `cargo fmt --check` clean · `clippy --all-targets -D warnings` exit 0 ·
  `cargo test` 64/64 (+1 live-GGUF) 
- Boot smoke `AI_EDITOR_SMOKE_OK` exit 0 · `cargo test -- --ignored` ✅

### Next step
P1-8 first-class subagents (`task` tool + restricted child perms +
`subagent_await`) — design survey already done (Option A: synchronous `task`
reusing pool handles with occupancy leasing + depth guard); or BN-9/BN-10
remainders (neural embedder, Voice Keyboard overlay).

---

## ✅ SHIPPED — Bionic backlog sweep: todos, web tools, sandboxed code, hardening, skills v2, MCP manager, model hub + API server (+ MCP env/allow-list finish), builds green

### What changed this pass (2026-08-22)

The tree contained a large untracked implementation pass (BN-2 through BN-8,
plus parts of BN-9/BN-10) left **uncompiled** mid-refactor: `McpServerConfig`
had grown `env` + `allowed_tools` fields whose callers were not updated.
This pass finished that refactor and verified everything:

**1. Compile fixes + allowed-tools enforcement (BN-7 finish)**
- `mcp.rs`: new `McpServerConfig::tool_allowed` / `matches_allow_list` — empty
  list allows everything, entries with a trailing `*` act as prefix wildcards;
  3 new unit tests (wildcard matching, JSON roundtrip of env/allowedTools).
- `tools.rs` `call_mcp_tool`: catalog lookup now carries the entry's `env`
  (passed to `McpHandle::spawn`) and enforces the allow-list before dispatch
  with a typed error naming the server + its filter. Ad-hoc `serverBin` calls
  stay unfiltered. Connection-cache keying unchanged (`bin args`).
- `tools.rs` `list_mcp_servers`: entries now surface `[allowed tools: …]`.
- `add_mcp_server`: initializes the new fields (empty) — the *model* cannot
  grant itself broad access; filters are user-managed.
- Frontend: `McpServerConfig` type gains optional `env` / `allowedTools`;
  SettingsModal shows an ⚑ badge with the filter count + tooltip.

**2. Verified already-implemented (this pass's audit)**

| Item | Evidence |
|---|---|
| BN-2 todos | `todo.rs`, 3 tools + schemas + default-allow, `.ai/todos.json`, orchestrator refuses to finish while items open (bounded nudges), live `TodoCard` via `agent://todo-update` |
| BN-3 web | `web_search` (DDG HTML parse, no API key), `web_extract` (text/* only, private hosts refused), `download_file` (≤100 MiB, NEW file only, approval EVERY call via `policy::always_ask`) |
| BN-4 sandboxed code | `run_python` (`-I`, scratchpad cwd, interpreter discovery), `run_javascript` (Deno lockdown flags, Node ≥20 fallback), typed error when absent |
| BN-5 hardening | YOLO sub-mode (`agent_set_yolo`; ROUTINE-only auto-approve, red-zone never), LLM shell-review second opinion rendered on approval buttons, `agent_grant_path` / `agent_revoke_path` per-session `{path, mode}` grants |
| BN-6 skills v2 | SKILL.md folder format (scripts/data alongside), global scope install, `skill_install` / `skill_uninstall` / `skill_set_active`, @-mention autocomplete in composer |
| BN-7 MCP manager | Persisted catalog (`mcp-servers.json`), `list/add/remove_mcp_server`, `agent_reset_mcp` reconnect, duplicate-name protection, env + allowed-tools filtering (this pass) |
| BN-8 hub + server | `hf_search` / `hf_download_model` (resume, cancel, progress events), Models tab UI; local OpenAI-compatible REST server (`v1/models`, `v1/chat/completions`, `v1/completions`, loopback-only) with start/stop/status + tab UI |

**Still open (deliberately scoped):**
- BN-9 remainder: neural (nomic-embed-class GGUF) embedder — attachments RAG
  currently ships dependency-free hashed n-gram embeddings.
- BN-10 remainder: Voice Keyboard overlay window — composer push-to-talk
  dictation (`voice_transcribe_data` + whisper) and the `transcribe_audio`
  tool are done.
- Vision companion (deferred from BN-11).

### Changed files (this pass)
- `src-tauri/src/agent/mcp.rs` — `tool_allowed` / `matches_allow_list` + 3 tests
- `src-tauri/src/agent/tools.rs` — catalog env carry-through, allow-list
  enforcement, listing surfaces filter, `add_mcp_server` init, BTreeMap import
- `src/types.ts` — `McpServerConfig.env?` / `allowedTools?`
- `src/components/SettingsModal.tsx` — allow-list badge on MCP rows
- Docs: `PROJECT_STATUS.md` / `ROADMAP.md`

### Next step
P1-8 (first-class subagents) or close out BN-9/BN-10 remainders (neural
embedder, Voice Keyboard overlay).

---

## ✅ SHIPPED — BN-11 UI polish + BN-12 packaging, builds green

- `cargo fmt` clean · `cargo clippy --all-targets -- -D warnings` exit 0
  (~30 pre-existing warnings fixed) · `cargo test` **62/62** (1 ignored
  live-GGUF) · `npm run build` green · **boot smoke verified locally**:
  `AI_EDITOR_SMOKE_OK`, exit 0.

### What changed this pass (2026-08-21/22)

**1. Multi-chat sessions (backend, main.rs)**
- Helpers `session_key` / `chat_key` / `session_file`; named chats persist to
  `sessions/<key>/<chat>.jsonl`, default chat keeps the legacy flat
  `<key>.jsonl`; `sessions/projects.json` maps keys → original paths.
- Commands: `session_append` / `session_load` (+ optional `chat_id`),
  `session_projects` (`SessionProjectInfo { key, name, lastActiveMs, chats[] }`
  incl. per-chat title/turns/updatedAtMs), `session_delete_chat`.
- Unit tests for key sanitization + file layout (62 total).

**2. Tray icon** — `build_tray()` (Show AI Editor / Quit, left-click shows),
tauri feature `tray-icon`.

**3. Headless boot smoke (BN-12)** — env `AI_EDITOR_SMOKE=1`: frontend probes
`smoke_active` then invokes `smoke_boot_ok` (prints `AI_EDITOR_SMOKE_OK`,
exit 0). Rust watchdog thread fails after 120s via `std::process::exit(1)`
(`AppHandle::exit(1)` proved unreliable — code was swallowed by teardown).
**Verified**: debug build requires the vite dev server (debug binaries serve
`devUrl`, not the embedded bundle); release builds embed `dist` and need
nothing else — that's what CI uses.

**4. CI (`.github/workflows/ci.yml`)**
- rust matrix job now runs `npm ci && npm run build` first — `generate_context!`
  embeds `../dist` at compile time, so fresh checkouts previously could not
  compile clippy/tests at all.
- New **boot-smoke** job (windows-latest): npm build → `cargo build --release`
  → launch with `AI_EDITOR_SMOKE=1` → require exit 0 + `AI_EDITOR_SMOKE_OK`
  (180s outer timeout > 120s watchdog).

**5. Clippy hygiene** — ~30 warnings fixed across engine/orchestrator/tools/
rag/plan/api-server (dead code, needless borrows, field renames with serde
rename_all preserving wire format).

### Changed files (this pass)
- Backend: `src-tauri/src/main.rs` (session commands, tray, smoke),
  `Cargo.toml` (tray-icon)
- Frontend: `src/App.tsx`, `src/main.tsx`, `src/types.ts`, `src/lib/ipc.ts`,
  `src/components/{ProjectsPanel,FileExplorer}.tsx`, `MenuBar.tsx`
- CI: `.github/workflows/ci.yml`
- Docs: `PROJECT_STATUS.md` / `ROADMAP.md`

### Next step
✅ BN-2 shipped 2026-08-22 (see top section). Remaining follow-ups: vision
companion (deferred from BN-11).

---

## BIONIC GAP ANALYSIS — vs `D:\software\Bionic\Bionic_Agent_Recreation_Prompt.txt`

Reviewed 2026-08-21. The prompt specifies "Nova", a privacy-first local agentic
AI workstation (Electron/Node in the spec). We map every requirement onto our
existing Tauri 2 / Rust / React stack — architecture is equivalent (Rust main
process hosts the agent engine; engine-pool worker threads replace utility
worker processes; JSONL journals replace SQLite). Per the prompt's own guidance,
implementation order is: §1–3 core agent → §4 skills → §5 RAG → §8 models/server
→ §6–7 MCP/voice → §9–10 polish. **§3.2 tool catalog + §3.3 permission model are
the heart — do not omit.**

### What we already satisfy (evidence)

| Bionic § | Requirement | Our implementation |
|---|---|---|
| §1 | Local-first inference, no telemetry | llama.cpp GGUF via `llama-cpp-2` (CPU; CUDA/Metal feature flags), optional OpenAI-compatible remote |
| §2.2 | Model formats, stop sequences, per-model configs | GGUF ✓, stop words ✓, gen params persisted ✓ |
| §2.3 | Session journal, streaming everything | JSONL session log + replay ✓; tokens/steps/tools/diffs streamed ✓ |
| §3.1 | ReAct loop, parallel calls, budgets, retries | orchestrator: generate→parse→dispatch→feedback, `join_all`, step budget, self-heal retry, stuck detection ✓ |
| §3.1 | Modes | Chat/Agent toggle + Plan mode ✓ (Coder+YOLO missing) |
| §3.2 FS | read_file_lines / find_files / search_content / edit_file | `read_file_range` / `glob_search_codebase` / `search_file_contents` / `apply_file_diff` (exact-match, error codes) ✓ |
| §3.2 Shell | shell_command w/ approval + destructive detector | `execute_terminal_command` + red-zone deny + permission modal ✓ |
| §3.2 Planning | todo list tools | partial — plan tools (`create_plan/read_plan/update_plan/execute_plan`) cover most |
| §3.2 Meta | skill create/read | `create_skill` / `read_skill` ✓ |
| §3.3 | Default deny outside workspace, approval UI, decision memory, audit | policy.json allow/ask/deny + red-zone + PermissionModal (once/session/always) + `.ai/audit.jsonl` ✓ |
| §4 | Skills w/ frontmatter + toggles | `.ai/skills/*.md` frontmatter skills + KnowledgePanel toggles ✓ (folder format/global scope missing) |
| §6 | MCP stdio client | `call_mcp_tool` (stdio JSON-RPC) ✓ |
| §9 | Transcript cards, token meter, checkpoints | tool cards, diff cards, step timeline, ctx gauge, CheckpointMenu ✓ |

### Gaps → BN backlog (prioritized per the prompt's build order)

**BN-1 — §3.2 filesystem completion (IN PROGRESS this pass)**: `list_dir`,
`read_file_chars`, `create_folder` (depth cap 50), `copy_file_or_folder`,
`move_file_or_folder` (`can_overwrite=false` default), `delete_file_or_folder`
(OS Trash), `get_scratchpad_folder` (per-session scratchpad OUTSIDE the
workspace; scratchpad paths exempt from workspace scoping).

**BN-2 — §3.2 planning todos**: `set_todo_list` / `get_todo_list` /
`mark_todo_item_done` rendered live in UI (= roadmap P1-7 goals & todos;
session cannot finish while items remain).

**BN-3 — §3.2 web tools**: `web_search`, `web_extract`, `download_file`
(public HTTP(S) only, ≤100 MiB, NEW file inside workspace, reject credentials +
private/loopback targets, approval EVERY call).

**BN-4 — §3.2 sandboxed code execution**: `run_python` / `run_javascript`.
Decision (documented in lieu of Pyodide/Deno bundling): discover interpreters
on PATH at call time; Deno runs with the spec's exact lockdown flag set
(`--allow-read=. --allow-write=. --no-prompt --deny-net --deny-env --deny-sys
--deny-run --deny-ffi`); Python runs isolated (`-I`) with cwd-scoped temp dir +
timeout; clear typed error when neither is installed.

**BN-5 — §3.3 hardening**: Coder mode + YOLO sub-mode (skip ROUTINE shell
approvals, never red-zone); independent LLM shell-approval reviewer pass;
per-session path grants `{path, mode}`.

**BN-6 — §4 skills upgrade**: SKILL.md folder format (scripts/data alongside),
global scope (`~/.ai/skills`), install/uninstall/enable/disable flows,
@-mention popup in composer.

**BN-7 — §6 MCP manager**: persisted server catalog (named servers, args),
restart/reconnect, allowed-tools filtering, duplicate-label protection.

**BN-8 — §8 model hub + API server**: HuggingFace download (URL/search,
resume), local OpenAI-compatible REST server over the loaded model
(v1/models, v1/chat/completions, v1/embeddings), headless serve mode.

**BN-9 — §5 RAG**: attachments pipeline, chunk/embed/cite (nomic-embed-class
GGUF), per-project vector index (upgrade from TF-IDF).

**BN-10 — §7 voice**: composer push-to-talk dictation (local Whisper ASR),
Voice Keyboard overlay typing into the focused app, `transcribe_file`.

**BN-11 — §9 UI polish**: projects/chats sidebar tree, vision companion
(text-only model + image consult), settings pages (MCP manager, server config,
voice wizard), tray icon.

**BN-12 — §10 packaging**: tauri-updater config, E2E smoke tests, CI.

Excluded as stack-specific Electron details (we keep Tauri): multi-window
webpack entries, Node worker processes, Squirrel updater, backend-extension
DLL manifests, MLX backend (macOS-only; our Metal feature covers it).

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
| P1-7 | Goals & todos tools (`set/get_todo_list`, `mark_todo_item_done`, live UI, finish-block) | ✅ done (2026-08-22) |
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
| BN-1 | Bionic §3.2 filesystem completion (7 tools: list_dir, read_file_chars, create_folder, copy/move/delete_file_or_folder, get_scratchpad_folder) | ✅ done (2026-08-21) |
| BN-2 | Bionic §3.2 todo-list tools (= P1-7) | ✅ done (2026-08-22) |
| BN-3 | Bionic §3.2 web tools (search/extract/download w/ SSRF guards) | ✅ done (2026-08-22) |
| BN-4 | Bionic §3.2 sandboxed run_python/run_javascript | ✅ done (2026-08-22) |
| BN-5 | Bionic §3.3 Coder+YOLO mode, LLM approval reviewer, per-session path grants | ✅ done (2026-08-22) |
| BN-6 | Bionic §4 SKILL.md folders + global scope + @-mentions | ✅ done (2026-08-22) |
| BN-7 | Bionic §6 MCP server catalog/manager (+ env, allowed-tools filter) | ✅ done (2026-08-22) |
| BN-8 | Bionic §8 HF model hub + local OpenAI-compatible API server | ✅ done (2026-08-22) |
| BN-9 | Bionic §5 RAG attachments/embeddings/citations | ◐ partial — hashed n-gram embedder shipped; neural GGUF embedder pending |
| BN-10 | Bionic §7 voice (dictation + Voice Keyboard) | ◐ partial — transcribe tool + composer dictation shipped; Voice Keyboard overlay pending |
| BN-11 | Bionic §9 UI polish (sidebar tree, settings pages, tray) — vision companion deferred | ✅ done (2026-08-22) |
| BN-12 | Bionic §10 updater + E2E boot smoke + CI | ✅ done (2026-08-22) |

---

## ✅ SHIPPED — BN-1 Bionic filesystem tool completion (7 new tools), builds green

- `cargo check` clean (only pre-existing warnings), `cargo test` **33/33**
  (1 ignored live-GGUF; was 24 — 9 new tests), `npm run build` green.

### What changed this pass (2026-08-21, BN-1)

**1. Seven new tools (Bionic §3.2 FILESYSTEM completion)**
- `list_dir(path?)` — dirs first then files, alphabetical, `/` markers +
  byte sizes, 2000-entry cap. Default-allow (read-only).
- `read_file_chars(path, offset?, limit?)` — UTF-8 **character-offset** reads
  for huge files / very long lines; default 4000, hard cap 24000 chars;
  result ends with `<EOF>` or an explicit continuation hint with the next
  offset. Default-allow.
- `create_folder(path)` — mkdir -p semantics, 50-segment depth cap
  (`folder_depth_ok`). Ask-policy.
- `copy_file_or_folder(src, dst, canOverwrite=false)` — recursive copy;
  refuses existing destination unless `canOverwrite` (then pre-clears it).
- `move_file_or_folder(...)` — same overwrite rule; same-volume rename fast
  path, cross-device fallback = copy + hard-delete source.
- `delete_file_or_folder(path)` — recursive delete to the **OS Trash** via the
  new `trash = "5"` crate; canonicalized guards refuse the workspace root and
  the `.ai` state folder.
- `get_scratchpad_folder()` — per-session scratchpad at
  `%TEMP%/ai-editor-scratchpad/session-<id>`, deliberately OUTSIDE the
  workspace; paths under the scratchpad root are **exempt from workspace
  scoping** in `policy::check` so temp files need no extra approvals.

**2. Policy & safety (Bionic §3.3 alignment)**
- `policy.rs`: `call_target_path` → `call_target_paths` (Vec) so copy/move
  scope **both endpoints**; relative paths now resolve against the workspace
  before scoping; scratchpad exemption; `default_allow` gains the three
  read-only tools.
- **Fixed a latent Windows bug**: `is_within` now strips the `\\?\`
  extended-length prefix after `canonicalize`, so brand-new files/folders
  (which don't canonicalize) no longer compare inconsistently against the
  existing workspace root — previously a new top-level file could be judged
  "outside the workspace".

**3. Model-facing docs**
- `prompt.ts`: tools 17–23 documented with JSON examples + usage guidance
  (explore with list_dir first; use the scratchpad for intermediates).

### Changed files (this pass)
- `src-tauri/Cargo.toml` — +`trash = "5"`
- `src-tauri/src/agent/mod.rs` — 7 ToolCall variants + names + summaries,
  `scratchpad_root()` / `session_scratchpad()`
- `src-tauri/src/agent/tools.rs` — implementations + dispatch arms + helpers
  (`abs_from`, `char_slice`, `folder_depth_ok`, `copy_recursive`,
  `clear_destination`) + 6 unit tests
- `src-tauri/src/agent/core.rs` — JSON schemas for the 7 tools
- `src-tauri/src/agent/policy.rs` — multi-path scoping, scratchpad exemption,
  `is_within` Windows-prefix fix, default_allow, 3 unit tests
- `src/lib/prompt.ts` — tools 17–23 in the system prompt
- `PROJECT_STATUS.md` — this log

### Next step
Smoke test `npm run tauri:dev`: ask the agent to "create a folder `tmp-demo`,
write a file into it, move it, then list the directory" → verify tool cards,
permission prompts on mutating ops only, and Trash recovery. Then **BN-2**
(todo-list tools = P1-7), which also completes Bionic §3.2 PLANNING.

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
code editor with 28 agentic tools.

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
- **BN-1 shipped (2026-08-21)**: Bionic §3.2 filesystem completion — 7 new tools
  (`list_dir`, `read_file_chars`, `create_folder`, `copy_file_or_folder`,
  `move_file_or_folder`, `delete_file_or_folder` → OS Trash,
  `get_scratchpad_folder`) + scratchpad scoping exemption + Windows
  `\\?\` path-normalization fix in `policy::is_within`. Tool count: **28**.
  This also closes the old P1-11 `delete` gap.
- **Next (highest ROI first)**:
  - **BN-2 = P1-7: Goals & todos** — Bionic §3.2 `set_todo_list` /
    `get_todo_list` / `mark_todo_item_done` rendered live in UI; session should
    not finish while items remain.
  - **P1-8 / BN-5**: first-class subagents (`task` + restricted child perms +
    `subagent_await`); Coder+YOLO mode + LLM shell-approval reviewer.
  - **P1-9**: `ask_question` / `send_to_user`.
  - **P1-10**: git blame/push/pull/branches/PR/CI.
  - **BN-3/BN-4**: web tools (search/extract/download w/ SSRF guards);
    sandboxed `run_python`/`run_javascript`.
- **Smoke test**: `npm run tauri:dev` — exercise the BN-1 tools end-to-end
  ("create a folder, write a file into it, move it, list the directory") and
  verify mutating ops prompt while read-only ops don't.

## Pending / next steps (ordered)

### Immediate — smoke test
1. `npm run tauri:dev` from `D:\ai`; load model → chat → agent task.
2. Verify BN-1 filesystem tools live (tool cards, permission prompts on
   mutating ops only, Trash recovery after delete).
3. `npm run tauri build` later for a production bundle (release build is slow;
   uses opt-level 3 + lto).

### Bionic backlog (see "BIONIC GAP ANALYSIS" at top) + legacy P-items
4. **BN-2 / P1-7** — todo-list tools (Bionic §3.2 PLANNING).
5. **BN-3** — web_search / web_extract / download_file (public HTTP(S) only,
   ≤100 MiB, new file in workspace, approval every call).
6. **BN-4** — sandboxed run_python / run_javascript.
7. **BN-5 / P1-8** — subagents + Coder/YOLO + LLM approval reviewer +
   per-session path grants.
8. **P1-9 / P1-10** — ask_question/send_to_user; git blame/push/pull/PR/CI.
   (P1-11 file-delete done via BN-1; remaining: read_lints/diagnostics.)
9. **BN-6…BN-12** — SKILL.md folders + global scope, MCP manager, HF model hub
   + local OpenAI-compatible API server, RAG, voice, UI polish, updater/E2E/CI.

### Legacy P2/P3 (unchanged)
10. **P2-12: Background work & multitasking** — `spawn_background_shell` /
    `background_subagent` that survive turn end, pill/badge UI, `abort_background_work`.
11. **P2-13: Session management UI** — `list_sessions`, `fork_session`, watch
    lifecycle, statuses `AWAITING_INPUT|ERROR|ABORTED`.
12. **P2-14: Modes** — `ASK` (every tool prompts), `DEBUG`, `CUSTOM` (per-mode
    system prompt + tool allowlist), `switch_mode`.
13. **P3-15: Context compaction** — at ~80% context summarize older messages into
    a `ConversationSummaryArchive` instead of hard-evicting.
14. **P3-16: Context usage tree + blob store** — per-component token contribution
    + blob store for large context.
15. **P3-17: Smart-mode classifier** — lightweight local risk classifier +
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
         ├─ tools.rs         (28 tool implementations + dispatch + audit)
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
