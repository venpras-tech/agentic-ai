import { useCallback, useEffect, useRef, useState } from "react";

import { api } from "../lib/ipc";
import type { FileNode } from "../types";

interface FileExplorerProps {
  workspaceRoot: string | null;
  workspaces?: string[];
  onSelectWorkspace: () => void;
  onAddWorkspace?: () => void;
  onRemoveWorkspace?: (root: string) => void;
  onOpenFile: (path: string) => void;
  onNewFile: () => void;
  onOpenSkills: () => void;
  /** Bumped whenever the agent touches the filesystem; triggers a re-list. */
  refreshSignal?: number;
  /** Manual refresh callback for the refresh button. */
  onRefresh?: () => void;
  /**
   * When true, renders only the tree body (no <aside>/header) for embedding
   * in the shared sidebar with the Chats/Files tab strip (BN-11).
   */
  chromeless?: boolean;
}

export default function FileExplorer(props: FileExplorerProps) {
  const {
    workspaceRoot,
    workspaces = [],
    onSelectWorkspace,
    onAddWorkspace,
    onRemoveWorkspace,
    onOpenFile,
    onNewFile,
    onOpenSkills,
    refreshSignal = 0,
    onRefresh,
    chromeless = false,
  } = props;
  const [roots, setRoots] = useState<FileNode[]>([]);
  const [children, setChildren] = useState<Record<string, FileNode[]>>({});
  const [expanded, setExpanded] = useState<Set<string>>(new Set());
  const [loading, setLoading] = useState(false);
  const [expanding, setExpanding] = useState<Set<string>>(new Set());
  const [refreshing, setRefreshing] = useState(false);

  useEffect(() => {
    if (!workspaceRoot) {
      setRoots([]);
      setChildren({});
      setExpanded(new Set());
      return;
    }
    setLoading(true);
    api
      .listDirectory(workspaceRoot)
      .then(setRoots)
      .catch(() => setRoots([]))
      .finally(() => setLoading(false));
  }, [workspaceRoot]);

  // Re-list the root (and any expanded folders) after agent-side edits so
  // newly created/renamed files appear without a manual collapse/expand.
  const expandedRef = useRef(expanded);
  expandedRef.current = expanded;
  useEffect(() => {
    if (!workspaceRoot || refreshSignal === 0) return;
    setRefreshing(true);
    const tasks: Promise<unknown>[] = [
      api.listDirectory(workspaceRoot).then(setRoots).catch(() => {}),
    ];
    for (const dir of Array.from(expandedRef.current)) {
      tasks.push(
        api
          .listDirectory(workspaceRoot, dir)
          .then((list) => setChildren((p) => ({ ...p, [dir]: list })))
          .catch(() => {}),
      );
    }
    Promise.all(tasks).finally(() => setRefreshing(false));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [refreshSignal, workspaceRoot]);

  const onToggle = useCallback(
    async (node: FileNode) => {
      if (!node.isDir) {
        onOpenFile(node.path);
        return;
      }
      const isOpen = expanded.has(node.path);
      if (!isOpen && !children[node.path]) {
        setExpanding((prev) => new Set(prev).add(node.path));
        try {
          const list = await api.listDirectory(workspaceRoot ?? "", node.path);
          setChildren((prev) => ({ ...prev, [node.path]: list }));
        } catch {
          setChildren((prev) => ({ ...prev, [node.path]: [] }));
        } finally {
          setExpanding((prev) => {
            const next = new Set(prev);
            next.delete(node.path);
            return next;
          });
        }
      }
      setExpanded((prev) => {
        const next = new Set(prev);
        if (isOpen) next.delete(node.path);
        else next.add(node.path);
        return next;
      });
    },
    [children, expanded, onOpenFile, workspaceRoot],
  );

  const renderNodes = (nodes: FileNode[], depth: number) =>
    nodes.map((node) => {
      const open = expanded.has(node.path);
      const isDir = node.isDir;
      const kids = children[node.path];
      return (
        <div key={node.path}>
          <div
            role="button"
            tabIndex={0}
            onClick={() => onToggle(node)}
            onKeyDown={(e) => e.key === "Enter" && onToggle(node)}
            className="group flex cursor-pointer items-center gap-1 rounded px-1 py-0.5 hover:bg-zinc-100"
            style={{ paddingLeft: depth * 12 + 6 }}
          >
            <span className="w-3 text-center text-[9px] text-zinc-400">
              {isDir ? (open ? "▾" : "▸") : ""}
            </span>
            <span className={`truncate text-[12px] ${isDir ? "font-medium text-zinc-700" : "text-zinc-500"}`}>
              {node.name}
            </span>
          </div>
          {isDir && open && kids && renderNodes(kids, depth + 1)}
          {isDir && open && expanding.has(node.path) && (
            <div className="space-y-0.5" style={{ paddingLeft: (depth + 1) * 12 + 6 }}>
              {[40, 56, 32].map((w, i) => (
                <div
                  key={i}
                  className="h-2.5 animate-pulse rounded bg-zinc-200"
                  style={{ width: `${w}%` }}
                />
              ))}
            </div>
          )}
        </div>
      );
    });

  const body = (
    <div className="min-h-0 flex-1 overflow-auto px-1.5 py-1.5">
      {!workspaceRoot ? (
        <div className="px-1 pt-3 text-center text-[11px] leading-relaxed text-zinc-400">
          No workspace open.
          <br />
          <button
            onClick={onSelectWorkspace}
            className="mt-2 rounded border border-border px-2 py-1 text-[11px] text-zinc-700 hover:border-zinc-400"
          >
            Open folder
          </button>
        </div>
      ) : loading ? (
        <div className="space-y-1 px-1">
          {[48, 64, 56, 40, 72, 52, 60, 44].map((w, i) => (
            <div
              key={i}
              className="h-3 animate-pulse rounded bg-zinc-200"
              style={{ width: `${w}%` }}
            />
          ))}
        </div>
      ) : roots.length === 0 ? (
        <p className="px-1 text-[11px] text-zinc-400">Empty folder</p>
      ) : (
        renderNodes(roots, 0)
      )}
    </div>
  );
  if (chromeless) return body;

  const multiRoot = workspaces.length > 1;
  const [showWsMenu, setShowWsMenu] = useState(false);

  return (
    <aside className="flex w-60 shrink-0 flex-col border-r border-border bg-panel">
      <div className="flex h-9 items-center justify-between border-b border-border px-2">
        <span className="text-[11px] font-semibold uppercase tracking-wider text-zinc-500">
          Explorer
        </span>
        <div className="flex items-center gap-1">
          {onRefresh && (
            <button
              onClick={onRefresh}
              title="Refresh explorer"
              aria-label="Refresh explorer"
              className={`rounded px-1.5 py-0.5 text-sm text-zinc-500 hover:bg-zinc-100 hover:text-zinc-800 ${refreshing ? "animate-spin" : ""}`}
            >
              ↻
            </button>
          )}
          <button
            onClick={onNewFile}
            title="New file"
            aria-label="New file"
            className="rounded px-1.5 py-0.5 text-sm text-zinc-500 hover:bg-zinc-100 hover:text-zinc-800"
          >
            +
          </button>
          <button
            onClick={onOpenSkills}
            title="Skills & rules"
            aria-label="Skills and rules"
            className="rounded px-1.5 py-0.5 text-sm text-zinc-500 hover:bg-zinc-100 hover:text-zinc-800"
          >
            ✦
          </button>
          <button
            onClick={onSelectWorkspace}
            title="Open workspace"
            aria-label="Open workspace"
            className="rounded px-1.5 py-0.5 text-sm text-zinc-500 hover:bg-zinc-100 hover:text-zinc-800"
          >
            📁
          </button>
        </div>
      </div>
      {multiRoot && (
        <div className="relative border-b border-border">
          <button
            onClick={() => setShowWsMenu(!showWsMenu)}
            className="flex w-full items-center gap-1 px-2 py-1 text-left text-[11px] text-zinc-600 hover:bg-zinc-100"
          >
            <span className="truncate flex-1" title={workspaceRoot ?? undefined}>
              {workspaceRoot?.split(/[/\\]/).pop() ?? "none"}
            </span>
            <span className="text-zinc-400">▾</span>
          </button>
          {showWsMenu && (
            <div className="absolute left-0 top-full z-50 min-w-full rounded border border-border bg-panel shadow-lg">
              {workspaces.map((ws) => {
                const name = ws.split(/[/\\]/).pop() ?? ws;
                const isPrimary = ws === workspaceRoot;
                return (
                  <div
                    key={ws}
                    className="flex items-center gap-1 px-2 py-1 text-[11px] hover:bg-zinc-100"
                  >
                    <button
                      className="flex-1 truncate text-left"
                      title={ws}
                      onClick={() => {
                        setShowWsMenu(false);
                        if (!isPrimary) onRemoveWorkspace?.(ws);
                        // To switch primary, user re-opens it via the folder picker.
                      }}
                    >
                      {isPrimary ? "● " : ""}{name}
                    </button>
                    {!isPrimary && onRemoveWorkspace && (
                      <button
                        className="shrink-0 text-zinc-400 hover:text-red-500"
                        title="Remove workspace"
                        onClick={() => {
                          setShowWsMenu(false);
                          onRemoveWorkspace(ws);
                        }}
                      >
                        ×
                      </button>
                    )}
                  </div>
                );
              })}
              {onAddWorkspace && (
                <button
                  className="flex w-full items-center gap-1 border-t border-border px-2 py-1 text-[11px] text-zinc-500 hover:bg-zinc-100"
                  onClick={() => {
                    setShowWsMenu(false);
                    onAddWorkspace();
                  }}
                >
                  + Add workspace
                </button>
              )}
            </div>
          )}
        </div>
      )}
      {body}
    </aside>
  );
}
