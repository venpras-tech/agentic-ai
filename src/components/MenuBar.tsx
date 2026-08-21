import { useCallback, useEffect, useRef, useState } from "react";

interface MenuBarProps {
  onOpenFolder: () => void;
  onOpenFile: () => void;
  onSettings: () => void;
  onSelectModel: () => void;
  onConsole: () => void;
  consoleVisible: boolean;
}

interface MenuDef {
  label: string;
  items: { label: string; shortcut?: string; action: () => void; active?: boolean }[];
}

export default function MenuBar(props: MenuBarProps) {
  const { onOpenFolder, onOpenFile, onSettings, onSelectModel, onConsole, consoleVisible } = props;
  const [open, setOpen] = useState<string | null>(null);
  const ref = useRef<HTMLDivElement>(null);

  const menus: MenuDef[] = [
    {
      label: "File",
      items: [
        { label: "Open Folder…", shortcut: "Ctrl+K Ctrl+O", action: onOpenFolder },
        { label: "Open File…", shortcut: "Ctrl+O", action: onOpenFile },
      ],
    },
    {
      label: "View",
      items: [
        { label: "Settings", shortcut: "Ctrl+,", action: onSettings },
        { label: "Select Model…", shortcut: "Ctrl+Shift+L", action: onSelectModel },
        { label: "Console", shortcut: "Ctrl+`", action: onConsole, active: consoleVisible },
      ],
    },
  ];

  const close = useCallback(() => setOpen(null), []);

  useEffect(() => {
    if (!open) return;
    const onDown = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) close();
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") close();
    };
    document.addEventListener("mousedown", onDown);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDown);
      document.removeEventListener("keydown", onKey);
    };
  }, [open, close]);

  return (
    <div
      ref={ref}
      className="flex h-7 shrink-0 items-center border-b border-border bg-panel px-2 select-none"
    >
      {menus.map((m) => (
        <div key={m.label} className="relative">
          <button
            onClick={() => setOpen(open === m.label ? null : m.label)}
            onMouseEnter={() => open && setOpen(m.label)}
            className={`rounded px-2 py-0.5 text-[11px] font-medium transition-colors ${
              open === m.label
                ? "bg-accent/15 text-accent"
                : "text-zinc-500 hover:bg-zinc-100 hover:text-zinc-800"
            }`}
          >
            {m.label}
          </button>
          {open === m.label && (
            <div className="absolute left-0 top-full z-50 mt-0.5 w-56 overflow-hidden rounded-md border border-border bg-panel-2 py-1 shadow-xl">
              {m.items.map((item) => (
                <button
                  key={item.label}
                  onClick={() => {
                    close();
                    item.action();
                  }}
                  className="flex w-full items-center gap-2 px-3 py-1.5 text-left text-[11.5px] text-zinc-700 hover:bg-accent/10 hover:text-accent"
                >
                  <span className="flex-1">{item.label}</span>
                  {item.active != null && (
                    <span className={`text-[10px] ${item.active ? "text-accent" : "text-zinc-400"}`}>
                      {item.active ? "●" : "○"}
                    </span>
                  )}
                  {item.shortcut && (
                    <span className="text-[10px] text-zinc-400 tabular-nums">{item.shortcut}</span>
                  )}
                </button>
              ))}
            </div>
          )}
        </div>
      ))}
    </div>
  );
}
