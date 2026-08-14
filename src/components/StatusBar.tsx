import type { ContextUsage, KnowledgeReport, ModelInfo, OpenFile } from "../types";

interface StatusBarProps {
  model: ModelInfo | null;
  workspaceRoot: string | null;
  activeFile: OpenFile | null;
  error: string | null;
  usage: ContextUsage | null;
  knowledge: KnowledgeReport | null;
}

function formatBytes(n: number): string {
  if (n >= 1 << 30) return `${(n / (1 << 30)).toFixed(1)} GB`;
  if (n >= 1 << 20) return `${(n / (1 << 20)).toFixed(0)} MB`;
  return `${n} B`;
}

function formatTokens(n: number): string {
  if (n >= 1000) return `${(n / 1000).toFixed(1)}k`;
  return String(n);
}

export default function StatusBar({ model, workspaceRoot, activeFile, error, usage, knowledge }: StatusBarProps) {
  return (
    <footer className="flex h-6 shrink-0 items-center justify-between gap-4 border-t border-border bg-panel px-3 text-[10.5px] text-zinc-500">
      <div className="flex min-w-0 items-center gap-3">
        {model ? (
          <>
            <span className="flex items-center gap-1">
              <span className="h-1.5 w-1.5 rounded-full bg-emerald-400" />
              {model.name}
            </span>
            <span className="hidden text-zinc-600 md:inline">
              {model.architecture} · {formatBytes(model.sizeBytes)} · {model.contextSize} ctx
            </span>
          </>
        ) : (
          <span className="flex items-center gap-1">
            <span className="h-1.5 w-1.5 rounded-full bg-zinc-600" />
            no model
          </span>
        )}
        {usage && (
          <span
            className={`tabular-nums ${
              usage.overflow ? "text-amber-300" : "text-zinc-600"
            }`}
            title={
              usage.overflow
                ? `Over ${formatTokens(usage.threshold)}-token budget (80% of ${formatTokens(usage.limit)}) - oldest turns are being evicted (${usage.evictedTurns} evicted so far)`
                : `${usage.messageCount} messages · ${formatTokens(usage.threshold)}-token eviction threshold`
            }
          >
            ctx {formatTokens(usage.totalTokens)}/{formatTokens(usage.limit)}
            {usage.overflow && <span className="text-amber-300"> · evicting</span>}
          </span>
        )}
        {knowledge && knowledge.skills.some((s) => s.active) && (
          <span className="text-zinc-600" title="Active skills (injected into context)">
            ✦ {knowledge.skills.filter((s) => s.active).length} skill
            {knowledge.skills.filter((s) => s.active).length === 1 ? "" : "s"}
          </span>
        )}
        {error && (
          <span className="truncate text-red-300" title={error}>
            ⚠ {error}
          </span>
        )}
      </div>
      <div className="flex min-w-0 shrink-0 items-center gap-3">
        {activeFile && (
          <span className="flex max-w-80 items-center gap-1.5 truncate">
            {!activeFile.saved && <span className="text-accent">●</span>}
            <span className="truncate">{activeFile.name}</span>
            <span className="truncate text-zinc-600">{activeFile.path}</span>
          </span>
        )}
        <span className="hidden max-w-60 truncate text-zinc-600 md:inline" title={workspaceRoot ?? undefined}>
          {workspaceRoot ?? "no workspace"}
        </span>
        <span>UTF-8</span>
        <span>Local AI Editor</span>
      </div>
    </footer>
  );
}
