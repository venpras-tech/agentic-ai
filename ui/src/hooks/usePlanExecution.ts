import { useCallback, useState } from "react";
import { useApp } from "../context/AppContext";
import { api, type AgentTaskRequest } from "../lib/ipc";
import { appendTurn, assistantTurnId, newTurnId } from "../lib/sessionLog";
import { buildPlanExecutionRequest } from "../lib/agentRequest";
import { setActiveChatId, setActiveTurnId } from "./useAgentStream";

export interface PendingPlan {
  request: AgentTaskRequest;
  turnId: string;
  workspace: string;
  chatId: string | null;
}

export interface UsePlanExecutionOptions {
  refreshContextUsage: () => void;
}

/** The approval flow for Plan mode: holds the awaiting-plan request and turns
 *  it into a real (non-plan) execution run when the user approves. */
export function usePlanExecution({ refreshContextUsage }: UsePlanExecutionOptions) {
  const {
    setProcessing,
    addMessage,
    addActivity,
    updateLastMessage,
    bumpHistory,
  } = useApp();
  const [pendingPlan, setPendingPlan] = useState<PendingPlan | null>(null);

  const discardPlan = useCallback(() => setPendingPlan(null), []);

  const executePlan = useCallback(async () => {
    const plan = pendingPlan;
    if (!plan) return;
    setPendingPlan(null);
    setProcessing(true);
    // The execution run is a turn of its own: distinct ids keep the reply out
    // of the plan's dedupe window, and the new id is what `useAgentStream`
    // will pair the assistant message with.
    const execTurnId = newTurnId();
    setActiveTurnId(execTurnId);
    setActiveChatId(plan.chatId);
    addMessage({ role: "assistant", content: "", streaming: true, turnId: assistantTurnId(execTurnId) });
    try {
      const request = buildPlanExecutionRequest(plan.request);
      request.prompt = "Execute the plan above now. Follow it step by step and verify as you go.";
      // `ContextManager` is the source of truth for the transcript and the
      // plan is already in it, so the approval instruction must be pushed too
      // or the second run would never see why it should act.
      await api.contextPushTurn("user", request.prompt).catch(() => {});
      await api.agentRunTask(request);
      refreshContextUsage();
      void appendTurn(
        plan.workspace,
        { role: "user", content: request.prompt, ts: Date.now(), turnId: execTurnId },
        plan.chatId,
        bumpHistory,
      );
      addActivity("Plan approved — executing", "status");
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      addActivity(msg, "error");
      updateLastMessage({ content: (c) => (c || "") + `\n[Error: ${msg}]`, streaming: false });
      setProcessing(false);
    }
  }, [
    pendingPlan,
    setProcessing,
    addMessage,
    addActivity,
    updateLastMessage,
    bumpHistory,
    refreshContextUsage,
  ]);

  return { pendingPlan, setPendingPlan, discardPlan, executePlan };
}