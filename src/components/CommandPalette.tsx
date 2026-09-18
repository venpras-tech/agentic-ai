import { useCallback, useEffect, useMemo, useState } from "react";
import { Command } from "cmdk";
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

interface Section {
  heading: string;
  items: GroupedItem[];
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

const GROUP_HEADING_CLASS =
  "[&_[data-cmdk-group-heading]]:px-3 [&_[data-cmdk-group-heading]]:pb-1 " +
  "[&_[data-cmdk-group-heading]]:pt-2 [&_[data-cmdk-group-heading]]:text-[10px] " +
  "[&_[data-cmdk-group-heading]]:font-semibold [&_[data-cmdk-group-heading]]:uppercase " +
  "[&_[data-cmdk-group-heading]]:tracking-wide [&_[data-cmdk-group-heading]]:text-text-dim";

const ITEM_CLASS =
  "flex w-full items-center gap-2 px-3 py-1.5 text-left text-sm text-text " +
  "data-[selected=true]:bg-accent/15";

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

  const load = useCallback(() => {
    setQuery("");
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
  }, [workspaceRoot]);

  useEffect(() => {
    if (open) load();
  }, [open, load]);

  const sections = useMemo<Section[]>(() => {
    const groups = buildGroups(
      initialMode,
      query,
      actions,
      files,
      sessions,
      skills,
      modes,
      activeMode,
      onOpenFile,
      onSwitchChat,
      onApplyMode,
      onToggleSkill,
    );
    const out: Section[] = [];
    for (const g of groups) {
      const last = out[out.length - 1];
      if (last && last.heading === g.group) last.items.push(g);
      else out.push({ heading: g.group, items: [g] });
    }
    return out;
  }, [initialMode, query, actions, files, sessions, skills, modes, activeMode, onOpenFile, onSwitchChat, onApplyMode, onToggleSkill]);

  if (!open) return null;

  const select = (item: GroupedItem) => {
    item.run();
    onClose();
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
        <Command
          onKeyDown={(e) => {
            if (e.key === "Escape") {
              e.preventDefault();
              onClose();
            }
          }}
          className="flex flex-col"
        >
          <div className="flex items-center gap-2 border-b border-panel-3 px-3 py-2.5">
            <span className="text-text-dim">⌕</span>
            <Command.Input
              value={query}
              onValueChange={setQuery}
              autoFocus
              placeholder={
                initialMode === "files"
                  ? "Open a file… (Ctrl+Shift+P for commands)"
                  : "Type a command, file, chat, skill, or mode…"
              }
              className="w-full flex-1 bg-transparent text-sm text-text focus:outline-none placeholder:text-text-dim"
            />
          </div>
          <Command.List className="max-h-[55vh] overflow-y-auto py-1">
            <Command.Empty className="px-4 py-6 text-center text-sm text-text-dim">
              No results
            </Command.Empty>
            {sections.map((sec) => (
              <Command.Group key={sec.heading} heading={sec.heading} className={GROUP_HEADING_CLASS}>
                {sec.items.map((item) => (
                  <Command.Item
                    key={item.key}
                    value={item.key}
                    keywords={item.keywords ? item.keywords.split(/\s+/) : undefined}
                    onSelect={() => select(item)}
                    className={ITEM_CLASS}
                  >
                    <span className="w-4 shrink-0 text-center text-xs text-text-dim">
                      {item.icon}
                    </span>
                    <span className="flex-1 truncate">{item.label}</span>
                    {item.hint && (
                      <span className="shrink-0 text-xs text-text-dim">{item.hint}</span>
                    )}
                  </Command.Item>
                ))}
              </Command.Group>
            ))}
          </Command.List>
          <div className="flex items-center gap-3 border-t border-panel-3 px-3 py-1.5 text-[10px] text-text-dim">
            <span>↑↓ navigate</span>
            <span>↵ select</span>
            <span>esc close</span>
          </div>
        </Command>
      </div>
    </div>
  );
}