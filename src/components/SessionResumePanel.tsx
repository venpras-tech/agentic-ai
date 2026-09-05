import { useCallback, useEffect, useState } from "react";

import { api } from "../lib/ipc";
import type { SessionProjectInfo } from "../types";

interface SessionResumePanelProps {
  onResume: (project: string, chatId: string | null) => void;
  onNewChat: () => void;
  workspaceRoot: string | null;
}

function relativeAge(ms: number): string {
  if (!ms) return "";
  const diff = Date.now() - ms;
  if (diff < 60_000) return "now";
  if (diff < 3_600_000) return `${Math.floor(diff / 60_000)}m`;
  if (diff < 86_400_000) return `${Math.floor(diff / 3_600_000)}h`;
  return `${Math.floor(diff / 86_400_000)}d`;
}

function baseName(path: string): string {
  const parts = path.split(/[\\/]/).filter(Boolean);
  return parts.length > 0 ? parts[parts.length - 1] : path;
}

/**
 * Session-resume picker (Phase 8): a keyboard-driveable list of every recent
 * chat across all projects, newest first, so users can re-open an earlier
 * conversation without digging through the projects tree. Mirrors Claude
 * `--resume` / Aider `/restore` / Zed's thread switcher.
 */
export default function SessionResumePanel({
  onResume,
  onNewChat,
  workspaceRoot,
}: SessionResumePanelProps) {
  const [projects, setProjects] = useState<SessionProjectInfo[]>([]);
  const [loading, setLoading] = useState(true);
  const [query, setQuery] = useState("");
  const [activeIndex, setActiveIndex] = useState(0);

  const refresh = useCallback(() => {
    setLoading(true);
    api
      .sessionProjects()
      .then(setProjects)
      .catch(() => setProjects([]))
      .finally(() => setLoading(false));
  }, []);

  useEffect(() => {
    refresh();
  }, [refresh]);

  // Flatten all chats across projects, newest first.
  const items = (() => {
    const all: {
      key: string;
      project: string;
      chatId: string | null;
      title: string;
      updatedAtMs: number;
      turns: number;
    }[] = [];
    for (const p of projects) {
      for (const c of p.chats) {
        all.push({
          key: `${p.key}:${c.id}`,
          project: p.name,
          chatId: c.id || null,
          title: c.title || "Default chat",
          updatedAtMs: c.updatedAtMs,
          turns: c.turns,
        });
      }
    }
    all.sort((a, b) => b.updatedAtMs - a.updatedAtMs);
    const q = query.trim().toLowerCase();
    const filtered = q
      ? all.filter(
          (i) =>
            i.title.toLowerCase().includes(q) ||
            baseName(i.project).toLowerCase().includes(q),
        )
      : all;
    return filtered.slice(0, 100);
  })();

  useEffect(() => {
    setActiveIndex((i) => Math.min(i, Math.max(items.length - 1, 0)));
  }, [items.length]);

  const run = (i: number) => {
    const item = items[i];
    if (!item) return;
    onResume(item.project, item.chatId);
  };

  const onKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === "ArrowDown") {
      e.preventDefault();
      setActiveIndex((i) => Math.min(i + 1, items.length - 1));
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      setActiveIndex((i) => Math.max(i - 1, 0));
    } else if (e.key === "Enter") {
      e.preventDefault();
      run(activeIndex);
    }
  };

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <div className="flex h-8 shrink-0 items-center justify-between border-b border-border px-2">
        <span className="text-[10px] font-semibold uppercase tracking-wider text-zinc-400">
          Resume a session
        </span>
        <button
          onClick={onNewChat}
          disabled={!workspaceRoot}
          aria-label="Start a new chat"
          title="Start a new chat in this project"
          className="rounded border border-border px-1.5 py-0.5 text-[10px] font-medium text-zinc-600 hover:border-accent/60 hover:text-cyan-700 disabled:opacity-40"
        >
          + New chat
        </button>
      </div>
      <div className="shrink-0 border-b border-border px-2 py-1">
        <input
          value={query}
          onChange={(e) => {
            setQuery(e.target.value);
            setActiveIndex(0);
          }}
          onKeyDown={onKeyDown}
          placeholder="Search sessions… (↑↓ to select, Enter to resume)"
          aria-label="Search sessions"
          className="w-full rounded border border-border bg-panel-2 px-2 py-0.5 text-[11px] text-ink outline-none placeholder:text-zinc-400 focus:border-accent/60"
        />
      </div>
      <div className="min-h-0 flex-1 overflow-auto px-1.5 py-1.5">
        {loading ? (
          <p className="px-1 text-[11px] text-zinc-400">Listing…</p>
        ) : items.length === 0 ? (
          <p className="px-1 pt-2 text-[11px] leading-relaxed text-zinc-400">
            {query ? "No matching sessions." : "No saved sessions yet."}
            <br />
            Send a message to start one.
          </p>
        ) : (
          items.map((item, i) => {
            const isCurrent =
              item.project === workspaceRoot;
            return (
              <div
                key={item.key}
                role="button"
                tabIndex={0}
                onClick={() => run(i)}
                onMouseEnter={() => setActiveIndex(i)}
                onKeyDown={(e) => e.key === "Enter" && run(i)}
                aria-label={`Resume ${item.title} in ${baseName(item.project)}`}
                className={`flex cursor-pointer items-center gap-1.5 rounded px-1.5 py-1 ${
                  i === activeIndex ? "bg-accent/15" : "hover:bg-zinc-100"
                }`}
              >
                <span className="w-3 shrink-0 text-center text-[9px] text-zinc-400">
                  {isCurrent ? "●" : "◔"}
                </span>
                <span className="min-w-0 flex-1">
                  <span className="block truncate text-[12px] text-zinc-700">
                    {item.title}
                  </span>
                  <span className="block truncate text-[9.5px] text-zinc-400">
                    {baseName(item.project)} · {item.turns} turn
                    {item.turns === 1 ? "" : "s"}
                  </span>
                </span>
                <span className="shrink-0 text-[9px] text-zinc-400">
                  {relativeAge(item.updatedAtMs)}
                </span>
              </div>
            );
          })
        )}
      </div>
      {items.length > 0 && (
        <div className="shrink-0 border-t border-border px-2 py-1 text-[9px] text-zinc-400">
          ↑↓ navigate · Enter resume
        </div>
      )}
    </div>
  );
}