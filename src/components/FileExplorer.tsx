import { useCallback, useEffect, useRef, useState } from "react";

import { api } from "../lib/ipc";
import type { FileNode } from "../types";

interface FileExplorerProps {
  workspaceRoot: string | null;
  onSelectWorkspace: () => void;
  onOpenFile: (path: string) => void;
  onNewFile: () => void;
  onOpenSkills: () => void;
  /** Bumped whenever the agent touches the filesystem; triggers a re-list. */
  refreshSignal?: number;
  /**
   * When true, renders only the tree body (no <aside>/header) for embedding
   * in the shared sidebar with the Chats/Files tab strip (BN-11).
   */
  chromeless?: boolean;
}

export default function FileExplorer(props: FileExplorerProps) {
  const {
    workspaceRoot,
    onSelectWorkspace,
    onOpenFile,
    onNewFile,
    onOpenSkills,
    refreshSignal = 0,
    chromeless = false,
  } = props;
  const [roots, setRoots] = useState<FileNode[]>([]);
  const [children, setChildren] = useState<Record<string, FileNode[]>>({});
  const [expanded, setExpanded] = useState<Set<string>>(new Set());
  const [loading, setLoading] = useState(false);

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
    api.listDirectory(workspaceRoot).then(setRoots).catch(() => {});
    for (const dir of Array.from(expandedRef.current)) {
      api
        .listDirectory(workspaceRoot, dir)
        .then((list) => setChildren((p) => ({ ...p, [dir]: list })))
        .catch(() => {});
    }
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
        try {
          const list = await api.listDirectory(workspaceRoot ?? "", node.path);
          setChildren((prev) => ({ ...prev, [node.path]: list }));
        } catch {
          setChildren((prev) => ({ ...prev, [node.path]: [] }));
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
        <p className="px-1 text-[11px] text-zinc-400">Listing…</p>
      ) : roots.length === 0 ? (
        <p className="px-1 text-[11px] text-zinc-400">Empty folder</p>
      ) : (
        renderNodes(roots, 0)
      )}
    </div>
  );
  if (chromeless) return body;

  return (
    <aside className="flex w-60 shrink-0 flex-col border-r border-border bg-panel">
      <div className="flex h-9 items-center justify-between border-b border-border px-2">
        <span className="text-[11px] font-semibold uppercase tracking-wider text-zinc-500">
          Explorer
        </span>
        <div className="flex items-center gap-1">
          <button
            onClick={onNewFile}
            title="New file"
            className="rounded px-1.5 py-0.5 text-sm text-zinc-500 hover:bg-zinc-100 hover:text-zinc-800"
          >
            +
          </button>
          <button
            onClick={onOpenSkills}
            title="Skills & rules"
            className="rounded px-1.5 py-0.5 text-sm text-zinc-500 hover:bg-zinc-100 hover:text-zinc-800"
          >
            ✦
          </button>
          <button
            onClick={onSelectWorkspace}
            title="Open workspace"
            className="rounded px-1.5 py-0.5 text-sm text-zinc-500 hover:bg-zinc-100 hover:text-zinc-800"
          >
            📁
          </button>
        </div>
      </div>
      {body}
    </aside>
  );
}
