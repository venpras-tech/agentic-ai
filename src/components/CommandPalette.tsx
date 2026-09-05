import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { api, isTauriRuntime } from "../lib/ipc";
import type { AgentMode, FileNode, SessionProjectInfo } from "../types";

export interface PaletteAction {
  id: string;
  label: string;
  hint?: string;
  keywords?: string;
  run: () => void;
  danger?: boolean;
}

export interface PaletteSkill {
  name: string;
  active: boolean;
}

interface GroupedItem {
  key: string;
  group: string;
  icon?: string;
  label: string;
  hint?: string;
  keywords?: string;
  danger?: boolean;
  run: () => void;
}

interface CommandPaletteProps {
  open: boolean;
  initialMode: "commands" | "files";
  onClose: () => void;
  workspaceRoot: string | null;
  actions: PaletteAction[];
  skills: PaletteSkill[];
  modes: AgentMode[];
  activeMode: string | null;
  onOpenFile: (path: string) => void;
  onSwitchChat: (project: string, chatId: string | null) => void;
  onApplyMode: (name: string | null) => void;
  onToggleSkill: (name: string, active: boolean) => void;
}

const SKIP_DIRS = new Set(["node_modules", ".git", "target", "dist", ".ai"]);

function buildGroups(
  mode: "commands" | "files",
  query: string,
  actions: PaletteAction[],
  files: string[],
  sessions: SessionProjectInfo[],
  skills: PaletteSkill[],
  modes: AgentMode[],
  activeMode: string | null,
  onOpenFile: (p: string) => void,
  onSwitchChat: (p: string, c: string | null) => void,
  onApplyMode: (n: string | null) => void,
  onToggleSkill: (n: string, a: boolean) => void,
): GroupedItem[] {
  const q = query.trim().toLowerCase();
  const match = (s: string) => !q || s.toLowerCase().includes(q);

  const groups: GroupedItem[] = [];
  const push = (items: GroupedItem[]) => {
    const filtered = items.filter(
      (i) => match(i.label) || (i.keywords && match(i.keywords)),
    );
    if (filtered.length) groups.push(...filtered);
  };

  if (mode === "commands" || q) {
    push(actions.map((a) => ({
      key: `a:${a.id}`,
      group: "Actions",
      icon: "▸",
      label: a.label,
      hint: a.hint,
      keywords: a.keywords,
      danger: a.danger,
      run: a.run,
    })));
  }

  if (mode === "files" || q) {
    push(files.map((f) => ({
      key: `f:${f}`,
      group: "Files",
      icon: "📄",
      label: f,
      keywords: f,
      run: () => onOpenFile(f),
    })));
  }

  if (q) {
    const chats: GroupedItem[] = [];
    for (const proj of sessions) {
      for (const c of proj.chats) {
        const title = c.title || "Default chat";
        const item: GroupedItem = {
          key: `s:${proj.key}:${c.id}`,
          group: "Sessions",
          icon: "💬",
          label: title,
          hint: proj.name,
          keywords: `${title} ${proj.key} ${proj.name}`,
          run: () => onSwitchChat(proj.name, c.id || null),
        };
        if (match(item.label) || match(item.keywords ?? "")) chats.push(item);
      }
    }
    if (chats.length) groups.push(...chats);
  }

  push(skills.map((s) => ({
    key: `sk:${s.name}`,
    group: "Skills",
    icon: s.active ? "✓" : "○",
    label: s.name,
    hint: s.active ? "active — click to disable" : "click to enable",
    keywords: s.name,
    run: () => onToggleSkill(s.name, !s.active),
  })));

  const modeItems: GroupedItem[] = modes.map((m) => ({
    key: `m:${m.name}`,
    group: "Agent modes",
    icon: m.name === activeMode ? "✓" : "◈",
    label: m.name,
    hint: m.description,
    keywords: `${m.name} ${m.description}`,
    run: () => onApplyMode(m.name === activeMode ? null : m.name),
  }));
  if (activeMode) {
    modeItems.unshift({
      key: "m:none",
      group: "Agent modes",
      icon: "○",
      label: "No custom mode",
      hint: "clear the active custom mode",
      keywords: "no custom mode clear",
      run: () => onApplyMode(null),
    });
  }
  if (q) {
    const filteredModes = modeItems.filter(
      (m) => match(m.label) || (m.keywords && match(m.keywords)),
    );
    if (filteredModes.length) groups.push(...filteredModes);
  }

  return groups;
}

export default function CommandPalette({
  open,
  initialMode,
  onClose,
  workspaceRoot,
  actions,
  skills,
  modes,
  activeMode,
  onOpenFile,
  onSwitchChat,
  onApplyMode,
  onToggleSkill,
}: CommandPaletteProps) {
  const [query, setQuery] = useState("");
  const [files, setFiles] = useState<string[]>([]);
  const [sessions, setSessions] = useState<SessionProjectInfo[]>([]);
  const [activeIndex, setActiveIndex] = useState(0);
  const inputRef = useRef<HTMLInputElement>(null);

  const load = useCallback(() => {
    setQuery("");
    setActiveIndex(0);
    if (isTauriRuntime() && workspaceRoot) {
      const walk = async (root: string, rel: string | null): Promise<string[]> => {
        const out: string[] = [];
        let nodes: FileNode[] = [];
        try {
          nodes = await api.listDirectory(root, rel);
        } catch {
          return out;
        }
        for (const n of nodes) {
          if (n.isDir) {
            if (SKIP_DIRS.has(n.name)) continue;
            out.push(...(await walk(root, n.path)));
          } else {
            out.push(n.path);
          }
        }
        return out;
      };
      void walk(workspaceRoot, null).then((f) => {
        if (f.length > 400) f = f.slice(0, 400);
        setFiles(f);
      });
    } else {
      setFiles([]);
    }
    if (isTauriRuntime()) {
      api.sessionProjects().then(setSessions).catch(() => setSessions([]));
    } else {
      setSessions([]);
    }
    requestAnimationFrame(() => inputRef.current?.focus());
  }, [workspaceRoot]);

  useEffect(() => {
    if (open) load();
  }, [open, load]);

  const groups = useMemo(
    () => buildGroups(initialMode, query, actions, files, sessions, skills, modes, activeMode, onOpenFile, onSwitchChat, onApplyMode, onToggleSkill),
    [initialMode, query, actions, files, sessions, skills, modes, activeMode, onOpenFile, onSwitchChat, onApplyMode, onToggleSkill],
  );

  const selectable = groups;

  useEffect(() => {
    setActiveIndex((i) => Math.min(i, Math.max(selectable.length - 1, 0)));
  }, [selectable.length]);

  if (!open) return null;

  const runIndex = (idx: number) => {
    const item = selectable[idx];
    if (!item) return;
    item.run();
    onClose();
  };

  const onKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === "ArrowDown") {
      e.preventDefault();
      setActiveIndex((i) => Math.min(i + 1, selectable.length - 1));
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      setActiveIndex((i) => Math.max(i - 1, 0));
    } else if (e.key === "Enter") {
      e.preventDefault();
      runIndex(activeIndex);
    } else if (e.key === "Escape") {
      e.preventDefault();
      onClose();
    }
  };

  return (
    <div
      className="fixed inset-0 z-50 flex items-start justify-center bg-black/40 pt-[15vh]"
      onMouseDown={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
    >
      <div
        className="flex w-[36rem] max-w-[92vw] flex-col overflow-hidden rounded-xl border border-panel-3 bg-panel-2 shadow-2xl"
        role="dialog"
        aria-modal="true"
        aria-label="Command palette"
      >
        <div className="flex items-center gap-2 border-b border-panel-3 px-3 py-2.5">
          <span className="text-text-dim">⌕</span>
          <input
            ref={inputRef}
            value={query}
            onChange={(e) => {
              setQuery(e.target.value);
              setActiveIndex(0);
            }}
            onKeyDown={onKeyDown}
            placeholder={
              initialMode === "files"
                ? "Open a file… (Ctrl+Shift+P for commands)"
                : "Type a command, file, chat, skill, or mode…"
            }
            className="flex-1 bg-transparent text-sm text-text focus:outline-none placeholder:text-text-dim"
          />
        </div>
        <div className="max-h-[55vh] overflow-y-auto py-1">
          {selectable.length === 0 ? (
            <div className="px-4 py-6 text-center text-sm text-text-dim">
              No results
            </div>
          ) : (
            (() => {
              let lastHeader = "";
              return selectable.map((g, i) => {
                const header = g.group !== lastHeader ? g.group : null;
                if (header) lastHeader = g.group;
                return (
                  <div key={g.key}>
                    {header && (
                      <div className="px-3 pb-1 pt-2 text-[10px] font-semibold uppercase tracking-wide text-text-dim">
                        {header}
                      </div>
                    )}
                    <button
                      type="button"
                      onMouseEnter={() => setActiveIndex(i)}
                      onClick={() => runIndex(i)}
                      className={`flex w-full items-center gap-2 px-3 py-1.5 text-left text-sm ${
                        i === activeIndex ? "bg-accent/15" : ""
                      }`}
                    >
                      <span className="w-4 shrink-0 text-center text-xs text-text-dim">
                        {g.icon}
                      </span>
                      <span className="flex-1 truncate">{g.label}</span>
                      {g.hint && (
                        <span className="shrink-0 text-xs text-text-dim">
                          {g.hint}
                        </span>
                      )}
                    </button>
                  </div>
                );
              });
            })()
          )}
        </div>
        <div className="flex items-center gap-3 border-t border-panel-3 px-3 py-1.5 text-[10px] text-text-dim">
          <span>↑↓ navigate</span>
          <span>↵ select</span>
          <span>esc close</span>
        </div>
      </div>
    </div>
  );
}
