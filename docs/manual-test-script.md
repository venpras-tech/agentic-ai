# Manual Test Script

Deliverable 2 of `todos.txt`. Run through in order. Prereqs: a local model
loaded (or a remote editor provider routed), a workspace folder opened, and the
app running with `npm run tauri dev` (or the packaged build).

## 0. Startup
- [ ] App boots; model status shows **loaded** in the status bar; no banner errors.
- [ ] Open a workspace (pick a small repo, e.g. this one). File tree populates;
      `.git`, `node_modules`, `target`, `dist` are absent.

## 1. Propose → accept (edit review loop)
1. On `/chat` (or `/workspace`), ask: *"Add a doc comment to the top of <file> and change the function name `oldName` to `newName`."*
2. Wait for an edit proposal → the review panel appears with a diff for the file
   (`agent://edit-proposed`).
   - [ ] Diff highlights `+`/`−` lines; buttons **Apply**, **Reject** visible per edit.
3. Click **Apply**.
   - [ ] `agent://edit-applied` fires; the editor pane (Workspace route) reloads the
         file and shows a green "File updated by the agent — reloaded from disk" toast.
   - [ ] The proposed chat segment flips to **applied** in the transcript.

## 2. Undo last agent changes (checkpoints)
4. In the editor pane header click **History**.
   - [ ] The panel lists the snapshot the agent took before applying (time + edit id).
5. Click **Restore** on that snapshot.
   - [ ] The file reverts to the pre-edit content; toast "Restored snapshot from …".
   - [ ] `original`/Context state resyncs (Ctrl+S shows "Saved", not "Save (Ctrl+S)").

### 2b. Rewind a turn (P5.1)
6. After a turn that edited a file, hover that assistant turn and click **Rewind**.
   - [ ] The dialog shows how many files the turn changed (e.g. "1 file").
7. Pick **Restore code and conversation** → **Rewind**.
   - [ ] The edited file reverts to its pre-turn content; messages after the anchor
         are dropped; activity log shows "Rewound conversation and 1 file …".
   - [ ] **Rewind** again on an earlier turn offers **Restore conversation only** and
         **Restore code only**; code modes are disabled when the turn changed nothing.

## 3. Mid-run route switch
6. Start a long agent task (e.g. *"explore the codebase and summarize all match
   handlers"*), and while it is running switch `/workspace` ↔ `/chat` and back.
   - [ ] Stream keeps rendering on the chat page; status banner stays "Running".
   - [ ] No duplicate activities / no gate re-open modal (the HITL text-parser is
         gone; gating is event-only).

## 4. Permission gate (non-YOLO ask)
7. Ensure YOLO is **off** on the Security page. Ask the agent to run a command not
   covered by any policy rule (e.g. `whoami` or a bare `curl`).
   - [ ] A permission request surfaces; choose **Allow once** and confirm the run;
         deny another and confirm the tool is skipped.

## 5. Search index (Phase 3)
8. Go to `/project`. In the search strip type a symbol name (e.g. `index_search`),
   press **Search** (text mode).
   - [ ] Status line shows indexed file/chars after the lazy build; results list
         `path:line` + preview.
9. Click a result.
   - [ ] Opens the file in the editor at that file (via `vencoder-open-editor`).
10. Toggle **Semantic** and search a phrase (e.g. "workspace text index").
    - [ ] Keyword-fallback stub returns the top files (best-effort; may mismatch).
11. Add an ignore line to `.gitignore` (e.g. `*.txt`), press **Rebuild index**.
    - [ ] `.gitignore`-matching files disappear from results.
12. Look at the app header chip (left of the top status): it shows file/char count
    and last-built time; clicking it re-indexes.
    - [ ] While rebuilding the chip shows a busy dot; opening a folder or editing a
          watched file auto-refreshes counts via `index://progress` /
          `workspace://file-changed`.
13. In the chat composer's `@` menu pick **Codebase** → a search box appears; type a
    symbol, click a hit.
    - [ ] The file path is added as a context chip.

## 5b. MCP servers (Settings → MCP)
14. In **Settings → MCP**, add a stdio server (e.g. `npx @modelcontextprotocol/server-filesystem <workspace>`).
15. Press **Test** on the server row.
    - [ ] Row shows `✓ N tools` (or a red error); tools are listed as checkboxes.
16. Uncheck one tool, press **Apply**.
    - [ ] With the server disabled **Test** is disabled; a disabled server (or an
          unchecked tool) makes `call_mcp_tool` return a "not enabled" error instead
          of running.
17. Use the sidebar **workspace switcher** (footer) to pick a previously opened
    folder; **Open folder…** browses for a new one.
    - [ ] Selecting a root navigates to `/project` and reindexes it; recent roots
          persist across restarts.
18. Apply an edit that breaks the build (or press **Re-run verify** in the
    edit-review panel).
    - [ ] A verify toast shows pass/fail with the command; the activity log records
          `agent://verify-result`. A failing verify nudges the agent to self-heal
          (up to 2 passes) and merges LSP diagnostics for the touched files.
19. Ask the agent: *"Use the explore subagent to find where session keys are
    built."*
    - [ ] The timeline gains a **Subagents** section showing the profile badge,
          a one-line summary and the duration once it finishes.
    - [ ] Clicking **View log** expands Backend activity filtered to that
          profile; the clear chip removes the filter.
    - [ ] Ask for several `task` calls in one reply when all workers are busy:
          a structured "All N spare engine worker(s) are busy" tool error appears
          (not a silent hang).
20. Kick off a background task (e.g. ask the agent to *run a long analysis in
    the background*), then click its **Background** pill in the timeline.
    - [ ] An output drawer opens under the pills and streams the task's lines
          (tool calls / assistant text) on a ~1s poll while it runs.
    - [ ] **Abort** in the drawer cancels the task; the pill turns to the
          aborted state and the drawer keeps the final lines.
    - [ ] Closing the drawer and reopening it still shows the retained output
          after the task has finished (ring buffer outlives the task).
21. In the composer, press **+ File** in the Attachments strip and pick a text
    file.
    - [ ] The file appears in the strip with its size and chunk count; asking
          the agent to *"search my attached files for X"* finds content in it.
    - [ ] Press **×** on the row - the entry disappears and a follow-up search
          no longer returns hits from that file.
22. In a named chat with several turns, hover an earlier message and press
    **Fork from here**.
    - [ ] A new chat opens immediately, showing the transcript up to and
          including that turn (and its reply).
    - [ ] The history list gains the new chat; the source chat is unchanged.
    - [ ] The fork button is absent on streaming messages and on the default
          (base) chat.
23. Ask mode hardening (P2.5): with a local model loaded, switch the composer
    to **Ask** and send a prompt like *"use write_file to create hello.txt"*.
    - [ ] The model may propose the tool, but the run never writes the file:
          the tool result in the trace reads "Ask mode forbids mutations".
    - [ ] Settings → Agent: enable **Ask uses chat completion**; the same
          prompt now returns a plain answer with no tool events at all.
    - [ ] Load a sub-3B local model, switch to **Agent** mode, type a prompt:
          the banner says Agent sending is disabled and pressing Enter shows
          an activity error instead of starting a run.
    - [ ] Switch to **Ask** on the same small model — sending works normally.

24. Workspace hooks (P3.1): in the workspace root create `.ai/hooks.json`:
    ```json
    {
      "hooks": {
        "session_start": [{ "command": "echo hooks-ready" }],
        "pre_tool_use": [
          { "command": "node -e \"const d=require('fs').readFileSync(0,'utf8');const j=JSON.parse(d);if(j.tool_name==='write_file')process.stdout.write(JSON.stringify({decision:'deny',reason:'hook blocked writes'}));else process.exit(0)\"" }
        ]
      }
    }
    ```
    - [ ] Start an Agent run → activity shows `Hook session_start ok — hooks-ready`
          and the run succeeds.
    - [ ] Ask the agent to create a file → the tool is denied before the policy
          gate; the activity shows `Hook pre_tool_use write_file deny — hook
          blocked writes`.
    - [ ] Delete `.ai/hooks.json` → the next run behaves normally (fail-open).

25. Agent automation API (P3.2): load a local model, open
    **Settings → LLM → Local model server**, start the server, then in a shell:
    ```sh
    curl -s http://127.0.0.1:8080/v1/models
    curl -s http://127.0.0.1:8080/v1/agent/tasks \
      -H "Content-Type: application/json" \
      -d '{"prompt":"list the files in the workspace","maxTokens":2048}'
    ```
    - [ ] `/v1/models` lists the loaded model; the task call returns
          `{"sessionId":<n>}` immediately.
    - [ ] The run's editor activity shows tool steps (as if started in-app).
    - [ ] `curl -s http://127.0.0.1:8080/v1/agent/sessions/<n>/trace` returns a
          `trace` array with one entry per step.
    - [ ] **Test connection** in Settings reports the model count.

## 6. Terminal drawer (Phase 3)
12. On `/workspace` bottom of the editor pane click **Terminal**, then **Spawn**.
    - [ ] Prompt shows the workspace cwd; type `echo hello` + Enter → `hello` prints.
13. Run something streaming (e.g. `ping -n 2 localhost`) → output streams live.
14. Press **Close** (or leave the page) → process is killed (no orphan shells).

26. Terminal @mention → composer (P3.3): with output in the drawer, drag-select
    some lines, then press the header **Mention** button.
    - [ ] The composer draft gains an `@terminal` fenced block with the selected
          text (appended, not replacing an existing draft).

27. Push-to-talk dictation (P3.3; needs local whisper + ffmpeg): press and **hold**
    the 🎤 button while speaking, then release.
    - [ ] Transcribed text is appended to the composer when the mic is released.
    - [ ] Denying mic permission shows a "Microphone unavailable" activity error.
    - [ ] Right-click or press **Esc** while recording → "Dictation cancelled",
          nothing is inserted.

## 7. Policy editor (Phase 3)
15. Open **Security** page → Policy tab.
    - [ ] Rules table shows user rules; the red-zone banner is read-only.
16. Change default to **deny**, add a rule `execute_terminal_command` → **allow**
    with pattern `git status`, press **Save policy**.
    - [ ] "Saved" appears; on **Refresh** the values persist (`.ai/policy.json`).
    - [ ] The file on disk has NO `__red_zone__` entry.

## 8. Regression pass
- [ ] `npm test` → 152 pass (14 files); `npx tsc --noEmit` clean.
- [ ] `cargo test` (in `src-tauri/`) → 275 pass, 4 ignored.

## 9. Chat parity (P5)

### 9a. Rewind / undo (P5.1)
17. Ask the agent to edit a file, then finish the turn.
18. Click **Rewind** on that turn → choose **code + conversation**. A "Rewound …" activity entry appears; the file reverts and the turn is dropped.
19. Type `/undo` (or press `Esc` twice with the transcript focused) → the last exchange is reverted (code restored, message removed) with an "Undid last turn" entry.
20. On the latest turn click **Summarize from here** → earlier turns collapse into one summary message; activity logs "Summarized earlier turns to free context".

### 9b. Change summary & diff ergonomics (P5.2)
21. After a turn that edits files, the assistant turn shows a **changed-files pill** (`+A −D across N files`); expanding lists each path with a status chip and **open**.
22. On an inline diff block, **accept** / **reject** act on that file only (same path as the review panel).
23. Live tool badges show **duration**, an **exit** code when present, and a collapsible **args** preview.
24. **Retry** on an assistant turn re-runs its prompt; **Trace · N** expands the per-step table.

### 9c. Approvals & modes (P5.3)
25. Security → Policy: toggle **auto-approve** categories (read/edit/shell/git-write/mcp/browser/other) and save; a matching tool then runs without a prompt. `always_ask` tools (e.g. `download_file`) and red-zone operations still prompt.
26. Permission prompt → **Don't ask again** persists an allow rule for that command.
27. In the composer press **Shift+Tab** → mode cycles agent → plan → ask (skipped while typing in a field).

### 9d. Composer context & mentions (P5.4)
28. Type `@` in the composer → a file typeahead opens; arrows navigate, Enter/Tab inserts the path, Esc closes. `@codebase` / `@git` / `@terminal` / `@web` insert the special tokens.
29. In assistant prose, a `path:line` reference renders as a clickable link that opens the editor at that line.

### 9e. Transcript navigation & empty state (P5.5)
30. Press **Ctrl+F** in the chat → a search bar filters the transcript to matching turns; Enter / ↑ ↓ step through hits with a `n/m` count; Esc closes.
31. The welcome/empty state offers **workspace-aware suggestions** (based on the open file) plus keybinding hints.
32. While a run is active, a sticky **task checklist** shows above the transcript and hides when all todos complete.

### 9f. Run board & queue (P5.6)
33. With subagent / background runs active, click **Runs · N** → a 4-column board (Running / Waiting / Done / Failed) shows live cards; **abort** on a running background card works; ✕ hides it.
34. Queue a message while the agent is busy, then use **↑ / ↓** to reorder and **✎** to edit it inline; **Steer** / **✕** still work.

## Known limits
- Terminal is line-oriented (no raw pty cursor control / no ANSI color parsing).
- Semantic search is a keyword stub, not embeddings.
- Per-hunk inline diff editing before apply (P5.6) is not implemented: the backend applies edits at file granularity (`agent_apply_edits` takes a group + edit ids), so there is no per-hunk apply primitive yet.
- `agent://skills-changed` fires only from the skill command handlers (not from
  agent-tool writes); the Settings → Skills list refreshes on the DOM event.
