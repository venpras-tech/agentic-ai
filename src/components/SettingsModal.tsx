import { useEffect, useState } from "react";

import type { GenParams } from "../types";

interface SettingsModalProps {
  open: boolean;
  onClose: () => void;
  params: GenParams;
  onParamsChange: (patch: Partial<GenParams>) => void;
}

export default function SettingsModal({
  open,
  onClose,
  params,
  onParamsChange,
}: SettingsModalProps) {
  const [local, setLocal] = useState(params);

  useEffect(() => {
    if (open) setLocal(params);
  }, [open, params]);

  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [open, onClose]);

  if (!open) return null;

  const apply = () => {
    onParamsChange(local);
    onClose();
  };

  return (
    <div
      className="fixed inset-0 z-40 flex items-center justify-center bg-black/30"
      role="dialog"
      aria-modal="true"
      aria-label="Settings"
    >
      <div className="flex w-[28rem] max-w-[92vw] flex-col gap-4 rounded-lg border border-border bg-panel-2 p-5 shadow-2xl">
        <div className="flex items-center justify-between">
          <span className="text-[13px] font-semibold text-ink">Settings</span>
          <button
            onClick={onClose}
            className="rounded p-1 text-zinc-500 hover:bg-zinc-100 hover:text-zinc-700"
          >
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
              <path d="M18 6 6 18M6 6l12 12" />
            </svg>
          </button>
        </div>

        <div className="grid grid-cols-2 gap-3">
          <label className="flex flex-col gap-1 text-[11px] text-zinc-500">
            Context Size
            <select
              value={local.contextSize}
              onChange={(e) => setLocal((p) => ({ ...p, contextSize: Number(e.target.value) }))}
              className="rounded border border-border bg-panel px-2 py-1 text-[12px] text-ink outline-none focus:border-accent/60"
            >
              {[2048, 4096, 8192, 16384, 32768].map((n) => (
                <option key={n} value={n}>{n}</option>
              ))}
            </select>
          </label>

          <label className="flex flex-col gap-1 text-[11px] text-zinc-500">
            Threads
            <input
              type="number"
              min={1}
              max={256}
              value={local.nThreads}
              onChange={(e) => setLocal((p) => ({ ...p, nThreads: Number(e.target.value) }))}
              className="rounded border border-border bg-panel px-2 py-1 text-[12px] text-ink outline-none focus:border-accent/60"
            />
          </label>

          <label className="flex flex-col gap-1 text-[11px] text-zinc-500">
            GPU Layers
            <input
              type="number"
              min={0}
              max={999}
              value={local.nGpuLayers}
              onChange={(e) => setLocal((p) => ({ ...p, nGpuLayers: Number(e.target.value) }))}
              className="rounded border border-border bg-panel px-2 py-1 text-[12px] text-ink outline-none focus:border-accent/60"
            />
          </label>

          <label className="flex flex-col gap-1 text-[11px] text-zinc-500">
            Max Tokens
            <input
              type="number"
              min={16}
              max={16384}
              step={16}
              value={local.maxTokens}
              onChange={(e) => setLocal((p) => ({ ...p, maxTokens: Number(e.target.value) }))}
              className="rounded border border-border bg-panel px-2 py-1 text-[12px] text-ink outline-none focus:border-accent/60"
            />
          </label>

          <label className="flex flex-col gap-1 text-[11px] text-zinc-500">
            Temperature
            <input
              type="number"
              min={0}
              max={2}
              step={0.05}
              value={local.temperature}
              onChange={(e) => setLocal((p) => ({ ...p, temperature: Number(e.target.value) }))}
              className="rounded border border-border bg-panel px-2 py-1 text-[12px] text-ink outline-none focus:border-accent/60"
            />
          </label>

          <label className="flex flex-col gap-1 text-[11px] text-zinc-500">
            Top P
            <input
              type="number"
              min={0}
              max={1}
              step={0.05}
              value={local.topP}
              onChange={(e) => setLocal((p) => ({ ...p, topP: Number(e.target.value) }))}
              className="rounded border border-border bg-panel px-2 py-1 text-[12px] text-ink outline-none focus:border-accent/60"
            />
          </label>
        </div>

        <p className="text-[9px] text-zinc-400">
          GPU and context settings take effect on the next model load. Temperature and top-p apply
          to every generation.
        </p>

        <div className="flex justify-end gap-2">
          <button
            onClick={onClose}
            className="rounded border border-border px-3 py-1.5 text-[11px] text-zinc-500 hover:bg-zinc-100 hover:text-zinc-700"
          >
            Cancel
          </button>
          <button
            onClick={apply}
            className="rounded bg-accent px-3 py-1.5 text-[11px] font-semibold text-white hover:bg-cyan-500"
          >
            Apply
          </button>
        </div>
      </div>
    </div>
  );
}
