interface DiffLine {
  tag: "header" | "add" | "del" | "ctx" | "meta";
  text: string;
}

function parseUnifiedDiff(raw: string): DiffLine[] {
  const lines = raw.split(/\r?\n/);
  const out: DiffLine[] = [];
  for (const line of lines) {
    if (line.startsWith("+++") || line.startsWith("---")) {
      out.push({ tag: "meta", text: line });
    } else if (line.startsWith("@@")) {
      out.push({ tag: "header", text: line });
    } else if (line.startsWith("+")) {
      out.push({ tag: "add", text: line });
    } else if (line.startsWith("-")) {
      out.push({ tag: "del", text: line });
    } else {
      out.push({ tag: "ctx", text: line });
    }
  }
  return out;
}

const TAG_CLASS: Record<DiffLine["tag"], string> = {
  header: "bg-cyan-500/10 text-cyan-600/90",
  add: "bg-emerald-500/10 text-emerald-600/90",
  del: "bg-red-500/10 text-red-600/90",
  ctx: "text-zinc-500",
  meta: "text-zinc-400",
};

interface DiffViewProps {
  path: string;
  diff: string;
}

export default function DiffView({ path, diff }: DiffViewProps) {
  const lines = parseUnifiedDiff(diff);
  const adds = lines.filter((l) => l.tag === "add").length;
  const dels = lines.filter((l) => l.tag === "del").length;
  const short = path.split(/[\\/]/).pop() ?? path;

  return (
    <div className="rounded-md border border-border bg-zinc-100">
      <div className="flex items-center justify-between gap-2 border-b border-border px-2 py-1">
        <span className="truncate font-mono text-[10px] text-zinc-500">{short}</span>
        <span className="shrink-0 text-[9px] tabular-nums text-zinc-400">
          <span className="text-emerald-500">+{adds}</span>{" "}
          <span className="text-red-500">-{dels}</span>
        </span>
      </div>
      <div className="max-h-64 overflow-auto py-1 font-mono text-[10px] leading-snug">
        {lines.map((l, i) => (
          <div key={i} className={`whitespace-pre-wrap px-2 ${TAG_CLASS[l.tag]}`}>
            {l.text || " "}
          </div>
        ))}
      </div>
    </div>
  );
}
