import { useEffect, useRef, useState } from "react";

import type { CheckpointInfo } from "../types";

interface CheckpointMenuProps {
  checkpoints: CheckpointInfo[];
  onCheckpoint: () => void;
  onRevert: (hash: string) => void;
}

export default function CheckpointMenu({
  checkpoints,
  onCheckpoint,
  onRevert,
}: CheckpointMenuProps) {
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    const onDoc = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) setOpen(false);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setOpen(false);
    };
    document.addEventListener("mousedown", onDoc);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDoc);
      document.removeEventListener("keydown", onKey);
    };
  }, [open]);

  return (
    <div ref={ref} className="relative">
      <button
        onClick={() => setOpen((v) => !v)}
        title="Git checkpoints — create a snapshot or one-click revert"
        className="flex items-center gap-1 rounded px-1.5 py-0.5 text-[10px] font-medium text-zinc-500 transition-colors hover:bg-panel hover:text-zinc-800"
      >
        ↺ {checkpoints.length}
      </button>
      {open && (
        <div className="absolute bottom-6 right-0 z-40 w-64 overflow-hidden rounded-lg border border-border bg-panel-2 shadow-2xl">
          <div className="border-b border-border px-3 py-2">
            <button
              onClick={() => {
                setOpen(false);
                onCheckpoint();
              }}
              className="w-full rounded bg-emerald-500/15 px-2 py-1.5 text-left text-[11px] font-semibold text-emerald-600 hover:bg-emerald-500/25"
            >
              ◆ Save checkpoint
            </button>
            <p className="mt-1.5 text-[9px] leading-snug text-zinc-400">
              Saves a tagged git commit of the whole workspace before destructive
              changes.
            </p>
          </div>
          <div className="max-h-64 overflow-y-auto p-1.5">
            {checkpoints.length === 0 ? (
              <p className="px-2 py-2 text-[10px] text-zinc-400">
                No checkpoints yet — create one above.
              </p>
            ) : (
              checkpoints.map((c) => (
                <button
                  key={c.hash}
                  onClick={() => {
                    setOpen(false);
                    onRevert(c.hash);
                  }}
                  title={`Revert workspace to ${c.subject}`}
                  className="flex w-full items-center gap-2 rounded px-2 py-1.5 text-left hover:bg-panel"
                >
                  <span className="truncate font-mono text-[9px] text-zinc-500">
                    {c.hash.slice(0, 7)}
                  </span>
                  <span className="min-w-0 flex-1 truncate text-[10.5px] text-zinc-700">
                    {c.subject.replace(/^checkpoint: /, "")}
                  </span>
                  <span className="shrink-0 text-[9px] text-zinc-400">{c.relative}</span>
                </button>
              ))
            )}
          </div>
          {checkpoints.length > 0 && (
            <div className="border-t border-border px-3 py-1.5 text-[9px] text-zinc-400">
              Click a checkpoint to hard-reset the workspace to it.
            </div>
          )}
        </div>
      )}
    </div>
  );
}
