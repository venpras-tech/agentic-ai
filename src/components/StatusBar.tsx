import type { ContextUsage, KnowledgeReport, LedgerEntry, ModelInfo, OpenFile } from "../types";
import AuditMenu from "./AuditMenu";
import CheckpointMenu from "./CheckpointMenu";

interface StatusBarProps {
  model: ModelInfo | null;
  workspaceRoot: string | null;
  workspaces?: string[];
  activeFile: OpenFile | null;
  error: string | null;
  usage: ContextUsage | null;
  knowledge: KnowledgeReport | null;
  ledger: LedgerEntry[];
  checkpoints: { hash: string; subject: string; relative: string }[];
  onCheckpoint: () => void;
  onRevert: (hash: string) => void;
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

function formatMs(n: number): string {
  if (n >= 60_000) return `${(n / 60_000).toFixed(1)}m`;
  if (n >= 1000) return `${(n / 1000).toFixed(1)}s`;
  return `${Math.round(n)}ms`;
}

/** Per-category colors for the context-breakdown bar (light theme). */
const CONTEXT_SEGMENTS: {
  key: keyof NonNullable<ContextUsage["breakdown"]>;
  label: string;
  cls: string;
}[] = [
  { key: "system", label: "system", cls: "bg-zinc-700" },
  { key: "file", label: "file buffer", cls: "bg-cyan-500" },
  { key: "rules", label: "rules", cls: "bg-violet-500" },
  { key: "skills", label: "skills", cls: "bg-emerald-500" },
  { key: "memory", label: "memory", cls: "bg-sky-500" },
  { key: "otherPinned", label: "pinned", cls: "bg-amber-500" },
  { key: "turns", label: "turns", cls: "bg-zinc-400" },
];

/** Horizontal segmented bar visualising the per-category context split. */
function ContextBreakdownBar({ usage }: { usage: ContextUsage }) {
  const b = usage.breakdown;
  if (!b) return null;
  const used = Math.max(1, usage.totalTokens);
  return (
    <span
      className="flex h-1.5 w-16 shrink-0 items-stretch overflow-hidden rounded-full bg-zinc-200"
      role="img"
      aria-label="Context usage by category"
      title={[
        "Context split (of " +
          `${formatTokens(usage.totalTokens)} used / ${formatTokens(usage.limit)} limit):`,
        ...CONTEXT_SEGMENTS.filter((s) => (b[s.key] ?? 0) > 0).map(
          (s) => `${s.label.padEnd(11)} ${formatTokens(b[s.key] ?? 0)}`,
        ),
        `evicted   ${usage.evictedTurns} turn(s)`,
      ].join("\n")}
    >
      {CONTEXT_SEGMENTS.filter((s) => (b[s.key] ?? 0) > 0).map((s) => (
        <span
          key={s.key}
          className={`${s.cls} h-full`}
          style={{ width: `${(((b[s.key] ?? 0) / used) * 100).toFixed(2)}%` }}
        />
      ))}
    </span>
  );
}

export default function StatusBar({
  model,
  workspaceRoot,
  workspaces = [],
  activeFile,
  error,
  usage,
  knowledge,
  ledger,
  checkpoints,
  onCheckpoint,
  onRevert,
}: StatusBarProps) {
  const totalTokens = ledger.reduce((n, l) => n + l.tokens, 0);
  const totalTools = ledger.reduce((n, l) => n + l.toolCalls, 0);
  const totalMs = ledger.reduce((n, l) => n + l.elapsedMs, 0);
  const hasLedger = ledger.length > 0;
  return (
    <footer className="flex h-6 shrink-0 items-center justify-between gap-4 border-t border-border bg-panel px-3 text-[10.5px] text-zinc-500">
      <div className="flex min-w-0 items-center gap-3">
        {model ? (
          <>
            <span className="flex items-center gap-1">
              <span className="h-1.5 w-1.5 rounded-full bg-emerald-400" />
              {model.name}
            </span>
            <span className="hidden text-zinc-400 md:inline">
              {model.architecture} · {formatBytes(model.sizeBytes)} · {model.contextSize} ctx
            </span>
          </>
        ) : (
          <span className="flex items-center gap-1">
            <span className="h-1.5 w-1.5 rounded-full bg-zinc-400" />
            no model
          </span>
        )}
        {hasLedger && (
          <span
            className="tabular-nums text-zinc-500"
            title={ledger
              .map(
                (l) =>
                  `${l.sessionId}: ${l.label} — ${formatTokens(l.tokens)} tok · ${l.toolCalls} tool(s) · ${formatMs(l.elapsedMs)}`,
              )
              .join("\n")}
          >
            Σ {ledger.length} session{ledger.length === 1 ? "" : "s"} ·{" "}
            {formatTokens(totalTokens)} tok · {totalTools} tool(s) · {formatMs(totalMs)}
          </span>
        )}
        {usage && (
          <span
            className={`tabular-nums ${
              usage.overflow ? "text-amber-600" : "text-zinc-400"
            }`}
            title={
              (usage.overflow
                ? `Over ${formatTokens(usage.threshold)}-token budget (80% of ${formatTokens(usage.limit)}) - oldest turns are being evicted (${usage.evictedTurns} evicted so far)`
                : `${usage.messageCount} messages · ${formatTokens(usage.threshold)}-token eviction threshold`)
              + (usage.breakdown
                  ? `\n\nContext split:\n${[
                      usage.breakdown.system > 0 && `system   ${formatTokens(usage.breakdown.system)}`,
                      usage.breakdown.file > 0 && `file     ${formatTokens(usage.breakdown.file)}`,
                      usage.breakdown.rules > 0 && `rules    ${formatTokens(usage.breakdown.rules)}`,
                      usage.breakdown.skills > 0 && `skills   ${formatTokens(usage.breakdown.skills)}`,
                      usage.breakdown.memory > 0 && `memory   ${formatTokens(usage.breakdown.memory)}`,
                      usage.breakdown.otherPinned > 0 && `pinned   ${formatTokens(usage.breakdown.otherPinned)}`,
                      `turns    ${formatTokens(usage.breakdown.turns)}`,
                    ].filter(Boolean).join("\n")}`
                  : "")
            }
          >
            ctx {formatTokens(usage.totalTokens)}/{formatTokens(usage.limit)}
            {usage.overflow && <span className="text-amber-600"> · evicting</span>}
            <ContextBreakdownBar usage={usage} />
          </span>
        )}
        {knowledge && knowledge.skills.some((s) => s.active) && (
          <span className="text-zinc-400" title="Active skills (injected into context)">
            ✦ {knowledge.skills.filter((s) => s.active).length} skill
            {knowledge.skills.filter((s) => s.active).length === 1 ? "" : "s"}
          </span>
        )}
        {error && (
          <span className="truncate text-red-600" title={error}>
            ⚠ {error}
          </span>
        )}
      </div>
      <div className="flex min-w-0 shrink-0 items-center gap-3">
        <CheckpointMenu
          checkpoints={checkpoints}
          onCheckpoint={onCheckpoint}
          onRevert={onRevert}
        />
        <AuditMenu />
        {activeFile && (
          <span className="flex max-w-80 items-center gap-1.5 truncate">
            {!activeFile.saved && <span className="text-accent">●</span>}
            <span className="truncate">{activeFile.name}</span>
            <span className="truncate text-zinc-400">{activeFile.path}</span>
          </span>
        )}
        <span className="hidden max-w-60 truncate text-zinc-400 md:inline" title={workspaceRoot ?? undefined}>
          {workspaceRoot ?? "no workspace"}
          {workspaces.length > 1 && (
            <span className="ml-1 text-zinc-500">(+{workspaces.length - 1})</span>
          )}
        </span>
        <span>UTF-8</span>
        <span>Local AI Editor</span>
      </div>
    </footer>
  );
}
