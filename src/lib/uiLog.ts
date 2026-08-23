/**
 * Frontend console logger with an explicit `[UI]` tag so output mixes cleanly
 * with the Rust backend's `[BE]` / `[LLM]` tagged lines
 * (`src-tauri/src/logging.rs`). Every line is mirrored into the in-app
 * Console window via `consoleBus`; devtools logging is dev-only.
 */

import { pushConsole } from "./consoleBus";

const ENABLED = import.meta.env.DEV;

export function uiLog(...args: unknown[]): void {
  const text = args
    .map((a) => (typeof a === "string" ? a : JSON.stringify(a) ?? String(a)))
    .join(" ");
  if (ENABLED) console.info("[UI]", text);
  pushConsole({ stream: "UI", tool: "chat", chunk: text });
}
