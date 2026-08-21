import { useEffect, useState } from "react";

import { api } from "../lib/ipc";
import type { KnowledgeReport } from "../types";

interface KnowledgePanelProps {
  open: boolean;
  onClose: () => void;
}

export default function KnowledgePanel({ open, onClose }: KnowledgePanelProps) {
  const [report, setReport] = useState<KnowledgeReport | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    if (!open) return;
    setBusy(true);
    setError(null);
    api
      .knowledgeReport()
      .then(setReport)
      .catch(() => setError("No workspace selected — open a folder first."))
      .finally(() => setBusy(false));
  }, [open]);

  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [open, onClose]);

  if (!open) return null;

  const toggle = async (name: string, active: boolean) => {
    setBusy(true);
    setError(null);
    try {
      const next = await api.skillSetActive(name, active);
      setReport(next);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const rescan = async () => {
    setBusy(true);
    setError(null);
    try {
      const next = await api.knowledgeScan();
      setReport(next);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const rulesEmpty = !report || report.rules.trim() === "";

  return (
    <div
      className="fixed inset-0 z-40 flex items-center justify-center bg-black/30"
      role="dialog"
      aria-modal="true"
      aria-label="Project skills and rules"
    >
      <div className="flex max-h-[80vh] w-[40rem] max-w-[94vw] flex-col gap-3 rounded-lg border border-border bg-panel-2 p-4 shadow-2xl">
        <div className="flex items-center justify-between">
          <span className="text-[13px] font-semibold text-ink">Skills &amp; Rules</span>
          <div className="flex items-center gap-2">
            <button
              onClick={rescan}
              disabled={busy}
              className="rounded border border-border px-2 py-1 text-[11px] text-zinc-500 hover:border-zinc-400 hover:text-zinc-800 disabled:opacity-40"
            >
              {busy ? "…" : "↻ Rescan"}
            </button>
            <button
              onClick={onClose}
              aria-label="Close"
              className="rounded p-1 text-zinc-500 hover:bg-panel hover:text-zinc-800"
            >
              <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
                <path d="M18 6 6 18M6 6l12 12" />
              </svg>
            </button>
          </div>
        </div>

        {error && (
          <p className="rounded border border-amber-500/30 bg-amber-500/10 px-3 py-2 text-[11px] text-amber-600">
            {error}
          </p>
        )}

        <div className="min-h-0 flex-1 overflow-y-auto">
          <div className="mb-3">
            <div className="mb-1 flex items-center justify-between">
              <span className="text-[10px] font-semibold uppercase tracking-wide text-zinc-500">
                Rules
              </span>
              {report && report.rulesSources.length > 0 && (
                <span className="text-[9px] text-zinc-400">
                  {report.rulesSources.join(" · ")}
                </span>
              )}
            </div>
            {rulesEmpty ? (
              <p className="rounded border border-border bg-panel px-3 py-2 text-[10px] text-zinc-500">
                No project rules yet. Create{" "}
                <span className="font-mono">.ai/rules/*.md</span>,{" "}
                <span className="font-mono">AGENTS.md</span>,{" "}
                <span className="font-mono">CLAUDE.md</span>, or{" "}
                <span className="font-mono">.cursorrules</span> in the workspace. Rules are
                always injected into the model context.
              </p>
            ) : (
              <pre className="max-h-40 overflow-y-auto rounded border border-border bg-panel px-3 py-2 text-[10px] leading-relaxed whitespace-pre-wrap text-zinc-500">
                {report!.rules}
              </pre>
            )}
          </div>

          <div>
            <div className="mb-1 flex items-center justify-between">
              <span className="text-[10px] font-semibold uppercase tracking-wide text-zinc-500">
                Skills
              </span>
              <span className="text-[9px] text-zinc-400">
                stored in <span className="font-mono">.ai/skills/*.md</span> (or global)
              </span>
            </div>
            {!report || report.skills.length === 0 ? (
              <p className="rounded border border-border bg-panel px-3 py-2 text-[10px] text-zinc-500">
                No skills found. Drop Markdown files with a{" "}
                <span className="font-mono">name</span>/<span className="font-mono">description</span>{" "}
                frontmatter into <span className="font-mono">.ai/skills/</span> and press ↻ Rescan.
              </p>
            ) : (
              <div className="flex flex-col gap-1.5">
                {report.skills.map((s) => (
                  <label
                    key={s.name}
                    className="flex cursor-pointer items-start gap-2.5 rounded border border-border bg-panel px-3 py-2 hover:border-zinc-400"
                  >
                    <input
                      type="checkbox"
                      checked={s.active}
                      disabled={busy}
                      onChange={(e) => void toggle(s.name, e.target.checked)}
                      className="mt-0.5 accent-cyan-400"
                    />
                    <span className="min-w-0 flex-1">
                      <span className="flex items-baseline justify-between gap-2">
                        <span className="text-[12px] font-semibold text-ink">{s.name}</span>
                        <span className="text-[9px] text-zinc-400">{s.source}</span>
                      </span>
                      {s.description && (
                        <span className="block text-[10px] text-zinc-500">{s.description}</span>
                      )}
                    </span>
                  </label>
                ))}
              </div>
            )}
          </div>
        </div>

        <p className="text-[9px] leading-snug text-zinc-400">
          Active skills are pinned into the model context like rules. Toggle to include or exclude
          them; only active skills reach the model.
        </p>
      </div>
    </div>
  );
}
