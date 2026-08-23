import { useEffect, useState } from "react";
import {
  isActivePhase,
  statusView,
  type ChatStatus,
} from "../lib/chatStatus";
import { uiLog } from "../lib/uiLog";

/** How long the green "Done" chip lingers before auto-hiding. */
const COMPLETE_HIDE_MS = 3500;
const TICK_MS = 500;

/**
 * Animated, accessible status line for the chat turn lifecycle.
 * Renders nothing while idle. Active phases tick a local clock so
 * elapsed/stale hints stay fresh; terminal phases freeze.
 */
export default function StatusIndicator({ status }: { status: ChatStatus }) {
  const [now, setNow] = useState(() => performance.now());
  const active = isActivePhase(status.phase);
  const lingering = status.phase === "complete";

  useEffect(() => {
    if (!active && !lingering) return;
    setNow(performance.now());
    const id = window.setInterval(() => setNow(performance.now()), TICK_MS);
    return () => window.clearInterval(id);
  }, [active, lingering, status.sinceMs]);

  // Mirror every turn-phase transition into the devtools console with the
  // same `[UI]` tag convention the Rust backend uses (`[BE]`, `[LLM]`).
  useEffect(() => {
    if (status.phase === "idle") return;
    uiLog(`chat phase → ${status.phase}${status.label ? ` (${status.label})` : ""}`);
  }, [status.phase, status.label]);

  if (status.phase === "idle") return null;
  if (lingering && now - status.sinceMs > COMPLETE_HIDE_MS) return null;

  const view = statusView(status, now);

  return (
    <div
      role="status"
      aria-live="polite"
      className={`flex shrink-0 items-center gap-2 border-t border-border px-3 py-1.5 text-xs ${
        view.stale ? "text-amber-600" : "text-zinc-500"
      }`}
    >
      <span aria-hidden="true" className="flex h-3 w-3 items-center justify-center">
        {status.phase === "streaming" ? (
          <Dots />
        ) : status.phase === "complete" ? (
          <CheckMark />
        ) : status.phase === "error" ? (
          <ErrorMark />
        ) : (
          <Spinner />
        )}
      </span>
      <span className="truncate">{view.label}</span>
      <span className="sr-only">{view.announcement}</span>
    </div>
  );
}

function Spinner() {
  return (
    <span className="block h-3 w-3 animate-spin rounded-full border-2 border-accent border-t-transparent" />
  );
}

function Dots() {
  return (
    <span className="flex gap-0.5">
      {[0, 1, 2].map((i) => (
        <span
          key={i}
          className="h-1 w-1 animate-bounce rounded-full bg-accent"
          style={{ animationDelay: `${i * 120}ms` }}
        />
      ))}
    </span>
  );
}

function CheckMark() {
  return <span className="block font-bold text-emerald-600">✓</span>;
}

function ErrorMark() {
  return <span className="font-bold text-red-500">✕</span>;
}
