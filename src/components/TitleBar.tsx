import { api } from "../lib/ipc";

const WINDOW_BUTTONS = [
  { key: "min", label: "Minimize", glyph: "─", onClick: () => api.minimize() },
  { key: "max", label: "Maximize", glyph: "□", onClick: () => api.toggleMaximize() },
  { key: "close", label: "Close", glyph: "✕", onClick: () => api.close() },
] as const;

export default function TitleBar() {
  return (
    <header
      className="flex h-9 shrink-0 items-center justify-between border-b border-border bg-panel select-none"
      data-tauri-drag-region
      onDoubleClick={() => api.toggleMaximize()}
    >
      <div className="flex items-center gap-2 pl-3" data-tauri-drag-region>
        <span className="flex h-3.5 w-3.5 items-center justify-center rounded-full bg-accent text-[9px] font-black text-black">
          AI
        </span>
        <span className="text-[11px] font-medium tracking-wide text-zinc-300">
          AI Editor
        </span>
        <span className="hidden text-[10px] text-zinc-600 sm:inline">
          local-first · llama.cpp
        </span>
      </div>
      <div className="flex h-full items-stretch">
        {WINDOW_BUTTONS.map((b) => (
          <button
            key={b.key}
            aria-label={b.label}
            onClick={b.onClick}
            className={`flex w-11 items-center justify-center text-[10px] text-zinc-400 transition-colors ${
              b.key === "close"
                ? "hover:bg-red-600 hover:text-white"
                : "hover:bg-white/10 hover:text-white"
            }`}
          >
            {b.glyph}
          </button>
        ))}
      </div>
    </header>
  );
}
