# AI Editor

Ultra-fast, low-memory, standalone **local-first AI code editor** — Tauri 2 +
Rust host process, React 19 + Monaco UI, and llama.cpp GGUF inference running
entirely on your machine (optional OpenAI-compatible remote backend).

Features include an agentic ReAct tool loop (filesystem / shell / search /
git / MCP tools), permission & policy system with audit log, plan & todo
tools, skills, RAG attachments, HF model hub, a local OpenAI-compatible REST
server, multi-chat sessions, git checkpoints, system tray, and a headless
boot smoke mode.

---

## 1. Required software

| Software | Version | Notes |
|---|---|---|
| **Node.js + npm** | ≥ 20 LTS | Vite 6 / Tauri CLI 2 |
| **Rust toolchain** | ≥ 1.77 (`rustup`) | MSVC toolchain on Windows |
| **C/C++ compiler** | per platform | Needed by llama.cpp (`llama-cpp-2`) on first build |

Platform-specific prerequisites (Tauri 2):

- **Windows**: Microsoft C++ Build Tools (MSVC) · WebView2 Runtime
  (preinstalled on Windows 10/11)
- **macOS**: Xcode Command Line Tools (`xcode-select --install`) · Metal is
  wired in automatically
- **Linux**: `libwebkit2gtk-4.1-dev`, `build-essential`, `curl`, `wget`,
  `file`, `libxdo-dev`, `libssl-dev`, `libayatana-appindicator3-dev`,
  `librsvg2-dev`

Optional (runtime-discovered, not build requirements):

- **Python 3** and/or **Deno** — power the sandboxed `run_python` /
  `run_javascript` agent tools
- **NVIDIA CUDA Toolkit** — only if you want the opt-in `gpu-cuda` feature
- **A GGUF model** (e.g. `qwen2.5-0.5b-instruct-q4_k_m.gguf`) placed in a
  local `models/` folder — or any OpenAI-compatible remote endpoint

## 2. Setup

```bash
# from the repository root
npm install          # frontend dependencies
```

Cargo crates (Tauri, tokio, llama.cpp bindings, …) are fetched automatically
on the first `cargo`/`tauri` invocation. The first build is slow — llama.cpp
compiles from source.

## 3. Running

### Development (HMR)

```bash
npm run tauri:dev
```

Starts the vite dev server on port 1420 and launches a debug binary that
serves `http://localhost:1420` with hot reload.

> Do **not** run the app with plain `npm run dev` — outside the Tauri shell
> all IPC is unavailable and the app shows a warning banner.

### Release build (production)

Option A — full installer/bundle via the Tauri CLI:

```bash
npm run tauri:build
```

Artifacts land in `src-tauri/target/release/bundle/`.

Option B — bare release binary:

```bash
npm run build
cargo build --release --features custom-protocol
```

Binary: `src-tauri/target/release/ai-editor.exe` (frontend embedded via the
`custom-protocol` feature).

> ⚠️ Plain `cargo build --release` **without** `custom-protocol` produces a
> binary that still expects the dev server on port 1420 and will fail offline
> / in smoke runs.

Optional GPU acceleration:

```bash
cargo build --release --features custom-protocol,gpu-cuda   # NVIDIA
# macOS builds use the Metal backend automatically
```

### Headless boot smoke test

```powershell
$env:AI_EDITOR_SMOKE = "1"
./src-tauri/target/release/ai-editor.exe
```

Prints `AI_EDITOR_SMOKE_OK` and exits 0 on success (120 s internal watchdog;
a failure prints `AI_EDITOR_SMOKE_FAIL: …` instead of hanging).

### Model setup

Launch the app → **View ▸ Select Model…** (Ctrl+Shift+L) and pick a `.gguf`
file, or configure a remote OpenAI-compatible endpoint in Settings.

## 4. Checks & tests

```bash
npx tsc --noEmit                       # frontend typecheck
npm run build                          # tsc + vite production bundle
cargo fmt --check                      # formatting
cargo clippy --all-targets -- -D warnings
cargo test                             # unit tests (live-GGUF test ignored)
cargo test -- --ignored                # live-GGUF headless chat (needs a model)
```

Note: `generate_context!` embeds `../dist` at compile time, so run
`npm run build` once before the first `cargo check`/`clippy`/`test` on a
fresh checkout.

## 5. Project layout

```
├── index.html               # frontend entry (+ boot-failure reporter)
├── src/                     # React UI (App, components, lib, hooks)
├── dist/                    # production bundle output (generated)
└── src-tauri/
    ├── src/main.rs          # Tauri commands, windows, tray, smoke harness
    ├── src/engine.rs        # llama.cpp engine pool
    ├── src/agent/           # orchestrator, tools, policy, plans, MCP, RAG…
    ├── capabilities/        # Tauri permission capabilities
    └── tauri.conf.json      # window/bundle configuration
```

Per-session state lives under `<workspace>/.ai/` (policy.json,
audit.jsonl, plan.json/md, skills/, todos.json, sessions/).
