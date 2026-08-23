/**
 * In-app console event bus.
 *
 * Single funnel for every console line shown in the Console window:
 * backend/LLM lines arrive via the `console-log` Tauri event (mirrored by
 * `logging.rs`), frontend lines via `uiLog`. App.tsx subscribes and renders;
 * components only ever publish, so nothing couples to React here.
 */

export interface ConsoleLine {
  /** Source tag: "UI" | "BE" | "LLM". */
  stream: string;
  /** Phase/category hint for filtering ("llm.step", "tool", "chat", …). */
  tool: string;
  chunk: string;
  ts: number;
}

type Listener = (line: ConsoleLine) => void;

const listeners = new Set<Listener>();

export function subscribeConsole(listener: Listener): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

export function pushConsole(line: Omit<ConsoleLine, "ts"> & { ts?: number }): void {
  const full: ConsoleLine = { ts: Date.now(), ...line };
  for (const l of listeners) l(full);
}

/** Parse a tagged Rust line (`[ts] [LEVEL] [TAG] [phase] msg`) into a line. */
export function parseBackendLine(raw: string): ConsoleLine {
  const tag = /\[(LLM|BE|UI)\]/.exec(raw)?.[1] ?? "BE";
  // Phase field is right-aligned to width 12: `[  llm.stream]`.
  const phase = /\[\s*(\w+\.\w+)\s*\]/.exec(raw)?.[1];
  return {
    stream: tag,
    tool: phase ?? "log",
    chunk: raw,
    ts: Date.now(),
  };
}
