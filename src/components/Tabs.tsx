import type { OpenFile } from "../types";

interface TabsProps {
  files: OpenFile[];
  activeKey: string | null;
  onSelect: (id: string) => void;
  onClose: (id: string) => void;
}

export default function Tabs({ files, activeKey, onSelect, onClose }: TabsProps) {
  if (files.length === 0) {
    return (
      <div className="flex h-9 shrink-0 items-center border-b border-border bg-panel px-3">
        <span className="text-[11px] text-zinc-400">Welcome — open a file to start editing</span>
      </div>
    );
  }

  return (
    <div className="flex h-9 shrink-0 items-stretch border-b border-border bg-panel">
      {files.map((f) => {
        const active = f.id === activeKey;
        return (
          <div
            key={f.id}
            onClick={() => onSelect(f.id)}
            role="tab"
            tabIndex={0}
            onKeyDown={(e) => e.key === "Enter" && onSelect(f.id)}
            className={`group flex max-w-52 cursor-pointer items-center gap-1.5 border-r border-border px-3 text-[12px] ${
              active
                ? "bg-editor font-medium text-ink"
                : "text-zinc-500 hover:bg-zinc-100 hover:text-zinc-700"
            }`}
          >
            {!f.saved && <span className="text-accent">●</span>}
            <span className="truncate">{f.name}</span>
            <button
              onClick={(e) => {
                e.stopPropagation();
                onClose(f.id);
              }}
              title="Close"
              className="rounded px-1 text-zinc-500 opacity-0 hover:bg-zinc-100 hover:text-zinc-800 group-hover:opacity-100"
            >
              ✕
            </button>
          </div>
        );
      })}
    </div>
  );
}
