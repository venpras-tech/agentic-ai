import { useEffect, useRef } from "react";

import type { PermissionRequest, PolicySnapshot } from "../types";

interface PermissionModalProps {
  request: PermissionRequest | null;
  policy: PolicySnapshot | null;
  onRespond: (requestId: string, allowed: boolean) => void;
}

function toolLabel(tool: string): string {
  switch (tool) {
    case "apply_file_diff":
      return "Edit a file";
    case "write_file":
      return "Write a file";
    case "execute_terminal_command":
      return "Run a terminal command";
    case "call_mcp_tool":
      return "Call an MCP tool";
    case "git_commit":
      return "Commit to git";
    case "git_checkpoint":
      return "Create a git checkpoint";
    case "git_revert":
      return "Revert to a checkpoint";
    default:
      return tool;
  }
}

export default function PermissionModal({ request, policy, onRespond }: PermissionModalProps) {
  const denyBtnRef = useRef<HTMLButtonElement>(null);

  useEffect(() => {
    if (request) denyBtnRef.current?.focus();
  }, [request]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (!request) return;
      if (e.key === "Escape") {
        e.preventDefault();
        onRespond(request.requestId, false);
      } else if (e.key === "Enter" && !e.shiftKey) {
        e.preventDefault();
        onRespond(request.requestId, true);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [request, onRespond]);

  if (!request) return null;

  return (
    <div
      className="fixed inset-0 z-40 flex items-center justify-center bg-black/60"
      role="dialog"
      aria-modal="true"
      aria-label="Permission request"
    >
      <div className="flex w-[30rem] max-w-[92vw] flex-col gap-3 rounded-lg border border-border bg-panel-2 p-4 shadow-2xl">
        <div className="flex items-center gap-2">
          <span className="flex h-6 w-6 items-center justify-center rounded-full bg-amber-500/20 text-[12px] text-amber-300">
            ⚠
          </span>
          <span className="text-[13px] font-semibold text-ink">
            {toolLabel(request.tool)} requires your approval
          </span>
        </div>

        <p className="rounded border border-border bg-panel px-3 py-2 font-mono text-[11px] leading-relaxed text-zinc-300">
          {request.summary}
        </p>

        {policy && (
          <p className="text-[10px] leading-snug text-zinc-500">
            Policy: {policy.default === "allow" ? "allowed by default" : "asks before mutating"} ·{" "}
            {policy.rules.filter((r) => r.tool !== "__red_zone__").length} custom rule
            {policy.rules.filter((r) => r.tool !== "__red_zone__").length === 1 ? "" : "s"} in{" "}
            <span className="font-mono">.ai/policy.json</span>
          </p>
        )}

        <div className="mt-1 flex items-center justify-between gap-3">
          <span className="text-[10px] text-zinc-600">
            Enter to allow · Esc to deny
          </span>
          <div className="flex items-center gap-2">
            <button
              ref={denyBtnRef}
              onClick={() => onRespond(request.requestId, false)}
              className="rounded border border-border px-4 py-1.5 text-[12px] font-medium text-zinc-300 hover:border-red-400/50 hover:text-red-300"
            >
              Deny
            </button>
            <button
              onClick={() => onRespond(request.requestId, true)}
              className="rounded bg-accent px-4 py-1.5 text-[12px] font-semibold text-black hover:bg-cyan-300"
            >
              Allow
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}
