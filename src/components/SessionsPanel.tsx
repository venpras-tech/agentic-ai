import type { BackgroundTaskInfo, LedgerEntry } from "../types";
import type { ChatStatus } from "../lib/chatStatus";
import type { SubtaskRun } from "../stores/agentRunStore";
import SessionResumePanel from "./SessionResumePanel";
import ThreadsPanel from "./ThreadsPanel";

interface SessionsPanelProps {
  tab: "resume" | "threads";
  onTabChange: (tab: "resume" | "threads") => void;
  workspaceRoot: string | null;
  onResume: (project: string, chatId: string | null) => void;
  onNewChat: () => void;
  chatStatus: ChatStatus;
  activeSessionId: number | null;
  currentStep: number | null;
  currentSubtask: SubtaskRun | null;
  /** Every sub-task still running (first-class `task` batches run in parallel). */
  runningSubtasks: SubtaskRun[];
  modelName: string | null;
  ledger: LedgerEntry[];
  backgroundTasks: BackgroundTaskInfo[];
  onAbort: (taskId: string) => void;
}

const TABS = [
  { id: "resume", label: "Resume" },
  { id: "threads", label: "Live" },
] as const;

/**
 * Unified sessions panel: past chats to resume and live thread / task status
 * live under one sidebar entry, so users don't have to pick between "history"
 * and "status" modes.
 */
export default function SessionsPanel({
  tab,
  onTabChange,
  workspaceRoot,
  onResume,
  onNewChat,
  chatStatus,
  activeSessionId,
  currentStep,
  currentSubtask,
  runningSubtasks,
  modelName,
  ledger,
  backgroundTasks,
  onAbort,
}: SessionsPanelProps) {
  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <div className="flex h-8 shrink-0 items-center gap-1 border-b border-border px-1.5">
        {TABS.map((t) => (
          <button
            key={t.id}
            onClick={() => onTabChange(t.id)}
            aria-pressed={tab === t.id}
            className={`rounded px-2 py-0.5 text-[10px] font-semibold uppercase tracking-wider transition-colors ${
              tab === t.id
                ? "bg-accent/15 text-cyan-600"
                : "text-zinc-500 hover:bg-zinc-100 hover:text-zinc-700"
            }`}
          >
            {t.label}
          </button>
        ))}
        <div className="flex-1" />
        <span className="text-[9px] text-zinc-400">{modelName ?? "no model"}</span>
      </div>
      {tab === "resume" ? (
        <SessionResumePanel
          workspaceRoot={workspaceRoot}
          onResume={onResume}
          onNewChat={onNewChat}
        />
      ) : (
        <ThreadsPanel
          chatStatus={chatStatus}
          activeSessionId={activeSessionId}
          currentStep={currentStep}
          currentSubtask={currentSubtask}
          runningSubtasks={runningSubtasks}
          modelName={modelName}
          ledger={ledger}
          backgroundTasks={backgroundTasks}
          onAbort={onAbort}
        />
      )}
    </div>
  );
}