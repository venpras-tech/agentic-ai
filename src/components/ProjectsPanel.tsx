import { useCallback, useEffect, useState } from "react";

import { api } from "../lib/ipc";
import type { SessionProjectInfo } from "../types";

interface ProjectsPanelProps {
  /** Original workspace path (or null when no workspace is open). */
  workspaceRoot: string | null;
  /** Currently open chat id; null = the default chat. */
  activeChatId: string | null;
  /** Bumped after appends/deletes so the tree re-lists. */
  refreshSignal: number;
  onSwitchChat: (project: string, chatId: string | null) => void;
  onNewChat: () => void;
  onDeleteChat: (project: string, chatId: string) => void;
  onOpenProject: (path: string) => void;
}

/** Mirrors the Rust `session_key` sanitization for current-project matching. */
function sessionKey(project: string): string {
  return project.replace(/[\\/\\:]/g, "_").replace(/^_+|_+$/g, "");
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

/** Projects/chats sidebar tree (BN-11): recent projects, their chats, and
 * chat switching. Data comes from the backend session log directory. */
export default function ProjectsPanel(props: ProjectsPanelProps) {
  const {
    workspaceRoot,
    activeChatId,
    refreshSignal,
    onSwitchChat,
    onNewChat,
    onDeleteChat,
    onOpenProject,
  } = props;
  const [projects, setProjects] = useState<SessionProjectInfo[]>([]);
  const [loading, setLoading] = useState(true);
  const [expanded, setExpanded] = useState<Set<string>>(new Set());
  const [search, setSearch] = useState("");

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

  // Re-list on external bumps (after sends / deletes).
  useEffect(() => {
    if (refreshSignal === 0) return;
    api.sessionProjects().then(setProjects).catch(() => {});
  }, [refreshSignal]);

  // Auto-expand the current workspace's project.
  useEffect(() => {
    if (!workspaceRoot) return;
    const key = sessionKey(workspaceRoot);
    setExpanded((prev) => {
      if (prev.has(key)) return prev;
      const next = new Set(prev);
      next.add(key);
      return next;
    });
  }, [workspaceRoot]);

  const toggle = useCallback((key: string) => {
    setExpanded((prev) => {
      const next = new Set(prev);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });
  }, []);

  const removeChat = useCallback(
    (e: React.MouseEvent, project: string, chatId: string) => {
      e.stopPropagation();
      if (
        !window.confirm(
          `Delete this chat?\n\n${baseName(project)} · ${chatId}\n\nThe log cannot be recovered.`,
        )
      )
        return;
      onDeleteChat(project, chatId);
    },
    [onDeleteChat],
  );

  const renderChats = (p: SessionProjectInfo) => {
    const q = search.toLowerCase();
    const filtered = q
      ? p.chats.filter(
          (c) =>
            c.title.toLowerCase().includes(q) ||
            (c.id && c.id.toLowerCase().includes(q)),
        )
      : p.chats;
    return filtered.map((chat) => {
      const isActive =
        p.name === (workspaceRoot ?? "default") &&
        (chat.id || null) === activeChatId;
      return (
        <div
          key={`${p.key}/${chat.id}`}
          role="button"
          tabIndex={0}
          onClick={() => onSwitchChat(p.name, chat.id || null)}
          onKeyDown={(e) =>
            e.key === "Enter" && onSwitchChat(p.name, chat.id || null)
          }
          title={`${chat.title}${chat.turns ? ` · ${chat.turns} turns` : ""}`}
          className={`group flex cursor-pointer items-center gap-1 rounded px-1 py-0.5 hover:bg-zinc-100 ${
            isActive ? "bg-accent/10" : ""
          }`}
          style={{ paddingLeft: 22 }}
        >
          <span
            className={`w-2 text-center text-[8px] ${
              isActive ? "text-cyan-600" : "text-zinc-300"
            }`}
          >
            ●
          </span>
          <span
            className={`min-w-0 flex-1 truncate text-[12px] ${
              isActive ? "font-medium text-cyan-700" : "text-zinc-500"
            }`}
          >
            {chat.title}
          </span>
          <button
            onClick={(e) => removeChat(e, p.name, chat.id)}
            title="Delete chat"
            className="hidden rounded px-1 text-[9px] text-zinc-400 hover:text-red-600 group-hover:block"
          >
            ✕
          </button>
          <span className="shrink-0 text-[9px] text-zinc-400">
            {relativeAge(chat.updatedAtMs)}
          </span>
        </div>
      );
    });
  };

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <div className="flex h-8 shrink-0 items-center justify-between border-b border-border px-2">
        <span className="text-[10px] font-semibold uppercase tracking-wider text-zinc-400">
          Recent projects
        </span>
        <button
          onClick={onNewChat}
          disabled={!workspaceRoot}
          title="Start a new chat in this project"
          className="rounded border border-border px-1.5 py-0.5 text-[10px] font-medium text-zinc-600 hover:border-accent/60 hover:text-cyan-700 disabled:opacity-40"
        >
          + New chat
        </button>
      </div>
      <div className="shrink-0 border-b border-border px-2 py-1">
        <input
          value={search}
          onChange={(e) => setSearch(e.target.value)}
          placeholder="Search chats…"
          className="w-full rounded border border-border bg-panel-2 px-2 py-0.5 text-[11px] text-ink outline-none placeholder:text-zinc-400 focus:border-accent/60"
        />
      </div>
      <div className="min-h-0 flex-1 overflow-auto px-1.5 py-1.5">
        {loading ? (
          <p className="px-1 text-[11px] text-zinc-400">Listing…</p>
        ) : projects.length === 0 ? (
          <p className="px-1 pt-2 text-[11px] leading-relaxed text-zinc-400">
            No chats yet.
            <br />
            Send a message to start one.
          </p>
        ) : (
          projects.map((p) => {
            const open = expanded.has(p.key);
            const isCurrent =
              !!workspaceRoot &&
              (p.name === workspaceRoot || p.key === sessionKey(workspaceRoot));
            return (
              <div key={p.key}>
                <div
                  role="button"
                  tabIndex={0}
                  onClick={() => toggle(p.key)}
                  onKeyDown={(e) => e.key === "Enter" && toggle(p.key)}
                  title={
                    p.name === "default"
                      ? "Chats without a workspace"
                      : `${p.name} — click to ${open ? "collapse" : "expand"}`
                  }
                  className="group flex cursor-pointer items-center gap-1 rounded px-1 py-0.5 hover:bg-zinc-100"
                >
                  <span className="w-3 text-center text-[9px] text-zinc-400">
                    {open ? "▾" : "▸"}
                  </span>
                  <span
                    className={`min-w-0 flex-1 truncate text-[12px] ${
                      isCurrent ? "font-medium text-zinc-700" : "text-zinc-500"
                    }`}
                  >
                    {p.name === "default"
                      ? "Sandbox (no workspace)"
                      : baseName(p.name)}
                  </span>
                  {p.name !== "default" && !isCurrent && (
                    <button
                      onClick={(e) => {
                        e.stopPropagation();
                        onOpenProject(p.name);
                      }}
                      title={`Open ${baseName(p.name)} as workspace`}
                      className="hidden rounded px-1 text-[10px] text-zinc-400 hover:text-cyan-700 group-hover:block"
                    >
                      📂
                    </button>
                  )}
                  <span className="shrink-0 rounded bg-panel-2 px-1 text-[9px] text-zinc-500">
                    {p.chats.length}
                  </span>
                </div>
                {open && renderChats(p)}
              </div>
            );
          })
        )}
      </div>
    </div>
  );
}
