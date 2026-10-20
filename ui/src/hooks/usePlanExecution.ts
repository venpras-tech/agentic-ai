import { useCallback, useState, type Dispatch, type SetStateAction } from "react";
import { useApp } from "../context/AppContext";
import { setActiveChatId, setActiveTurnId } from "../context/AgentStreamContext";
import { api, type AgentTaskRequest } from "../lib/ipc";
import { buildPlanExecutionRequest } from "../lib/agentRequest";
import { appendTurn, newTurnId } from "../lib/sessionLog";

/** A plan run that finished and is waiting for the user to approve execution. */
export interface PendingPlan {
  request: AgentTaskRequest;
  turnId: string;
  workspace: string;
  chatId: string | null;
}

export interface PlanExecution {
  pendingPlan: PendingPlan | null;
  setPendingPlan: Dispatch<SetStateAction<PendingPlan | null>>;
  discardPlan: () => void;
  executePlan: () => Promise<void>;
}

/**
 * Plan → Act separation: the backend deliberately refuses to edit while
 * `planMode` is set, so the plan is parked here, shown to the user, and only
 * turned into a second run once they approve it.
 */
export function usePlanExecution(): PlanExecution {
  const { setProcessing, addMessage, addActivity, updateLastMessage } = useApp();
  const [pendingPlan, setPendingPlan] = useState<PendingPlan | null>(null);

  // A pending plan stays mounted while its run streams: the approval row is
  // already gated on `!isProcessing`, so clearing it here would discard the
  // plan the moment it was created. A fresh submission clears it instead.
  const discardPlan = useCallback(() => setPendingPlan(null), []);

  const executePlan = useCallback(async () => {
    const plan = pendingPlan;
    if (!plan) return;
    setPendingPlan(null);
    setProcessing(true);
    // The execution run is a turn of its own: distinct ids keep the reply out
    // of the plan's dedupe window, and the new id is what the agent-stream
    // provider will pair the assistant message with.
    const execTurnId = newTurnId();
    setActiveTurnId(execTurnId);
    setActiveChatId(plan.chatId);
    addMessage({ role: "assistant", content: "", streaming: true });
    try {
      const request = buildPlanExecutionRequest(plan.request);
      request.prompt = "Execute the plan above now. Follow it step by step and verify as you go.";
      // `ContextManager` is the source of truth for the transcript and the
      // plan is already in it, so the approval instruction must be pushed too
      // or the second run would never see why it should act.
      await api.contextPushTurn("user", request.prompt).catch(() => {});
      await api.agentRunTask(request);
      void appendTurn(
        plan.workspace,
        { role: "user", content: request.prompt, ts: Date.now(), turnId: execTurnId },
        plan.chatId,
      );
      addActivity("Plan approved — executing", "status");
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      addActivity(msg, "error");
      updateLastMessage({ content: (c) => (c || "") + `\n[Error: ${msg}]`, streaming: false });
      setProcessing(false);
    }
  }, [pendingPlan, setProcessing, addMessage, addActivity, updateLastMessage]);

  return { pendingPlan, setPendingPlan, discardPlan, executePlan };
}
