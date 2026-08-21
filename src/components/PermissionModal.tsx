import { useEffect, useRef } from "react";

import type { PermissionRequest, PolicySnapshot } from "../types";

interface PermissionModalProps {
  request: PermissionRequest | null;
  policy: PolicySnapshot | null;
  onRespond: (requestId: string, decision: string) => void;
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
        onRespond(request.requestId, "deny");
      } else if (e.key === "Enter" && !e.shiftKey) {
        e.preventDefault();
        onRespond(request.requestId, "allow_once");
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [request, onRespond]);

  if (!request) return null;

  return (
    <div
      className="fixed inset-0 z-40 flex items-center justify-center bg-black/40"
      role="dialog"
      aria-modal="true"
      aria-label="Permission request"
    >
      <div className="flex w-[30rem] max-w-[92vw] flex-col gap-3 rounded-lg border border-border bg-panel-2 p-4 shadow-2xl">
        <div className="flex items-center gap-2">
          <span className="flex h-6 w-6 items-center justify-center rounded-full bg-amber-500/20 text-[12px] text-amber-600">
            ⚠
          </span>
          <span className="text-[13px] font-semibold text-ink">
            {toolLabel(request.tool)} requires your approval
          </span>
        </div>

        <p className="rounded border border-border bg-panel px-3 py-2 font-mono text-[11px] leading-relaxed text-zinc-700">
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

        <div className="grid grid-cols-2 gap-2">
          <button
            ref={denyBtnRef}
            onClick={() => onRespond(request.requestId, "deny")}
            className="rounded border border-border px-3 py-1.5 text-[12px] font-medium text-zinc-700 hover:border-red-400/50 hover:text-red-600"
          >
            Deny
          </button>
          <button
            onClick={() => onRespond(request.requestId, "allow_once")}
            className="rounded bg-accent px-3 py-1.5 text-[12px] font-semibold text-white hover:bg-cyan-500"
          >
            Allow once
          </button>
          <button
            onClick={() => onRespond(request.requestId, "allow_session")}
            title="Allow every call of this tool (this exact command for terminal) for the rest of the session"
            className="rounded border border-emerald-500/30 bg-emerald-500/10 px-3 py-1.5 text-[12px] font-medium text-emerald-600 hover:bg-emerald-500/20"
          >
            Allow for session
          </button>
          <button
            onClick={() => onRespond(request.requestId, "always_allow")}
            title="Write an allow rule to .ai/policy.json so this tool never asks again"
            className="rounded border border-emerald-500/30 bg-emerald-500/10 px-3 py-1.5 text-[12px] font-medium text-emerald-600 hover:bg-emerald-500/20"
          >
            Always allow
          </button>
        </div>

        <p className="text-[10px] text-zinc-400">
          Enter = allow once · Esc = deny. "Always allow" writes a rule to{" "}
          <span className="font-mono">.ai/policy.json</span>; session memory lasts for this run.
        </p>
      </div>
    </div>
  );
}
