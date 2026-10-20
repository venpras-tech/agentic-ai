import { useCallback, useEffect, type Dispatch, type SetStateAction } from "react";
import { useNavigate, useLocation } from "react-router-dom";
import { useApp } from "../context/AppContext";
import { setActiveChatId, setActiveTurnId, useAgentStream } from "../context/AgentStreamContext";
import { api } from "../lib/ipc";
import { isAgentCapableLocalModel, prepareAgentTurn } from "../lib/agentPrepare";
import { buildAgentRequest } from "../lib/agentRequest";
import { appendTurn, newTurnId, resolveChatId } from "../lib/sessionLog";
import type { PendingPlan } from "./usePlanExecution";

export interface ComposerSendOptions {
  customMode: string;
  activeWorkflow: string;
  runInBackground: boolean;
  discardPlan: () => void;
  setPendingPlan: Dispatch<SetStateAction<PendingPlan | null>>;
}

/**
 * Owns the whole "turn" pipeline: resolve a model, sync the backend context,
 * build the agent request, start (or detach) the run and persist the user turn.
 * Returns a single stable `sendPrompt` so the steer queue and the
 * `vencoder-send-prompt` window event can both drive it.
 */
export function useComposerSend(opts: ComposerSendOptions) {
  const { customMode, activeWorkflow, runInBackground, discardPlan, setPendingPlan } = opts;
  const navigate = useNavigate();
  const location = useLocation();
  const { refreshContextUsage } = useAgentStream();
  const {
    setComposerDraft,
    currentMode,
    setProcessing,
    verify,
    maxSteps,
    addMessage,
    updateLastMessage,
    addActivity,
    clearActivity,
    contextPaths,
    projectPath,
    setProjectPath,
    contextState,
    activeFile,
    settings,
    currentConversationId,
    modelName,
    modelProvider,
    setModelInfo,
    modelLoading,
    modelRestoreState,
    modelMeta,
    promptModelSelection,
    rememberLastModel,
  } = useApp();

  const sendPrompt = useCallback(
    async (text: string) => {
      const t = text.trim();
      if (!t) return;

      // On Enter, make sure a model is actually loaded and ready before
      // running: "No model loaded" would otherwise surface as an error mid-run.
      if (modelLoading) {
        addActivity("Model is still loading — wait for the banner before sending.", "status");
        return;
      }
      let effectiveModel = { name: modelName, provider: modelProvider };
      if (!modelName || modelRestoreState === "failed") {
        const routed = (await api.providersRoute("editor").catch(() => null)) as
          | Record<string, unknown>
          | null;
        const kind = routed ? String(routed.kind ?? "") : "";
        const display = routed ? String(routed.name ?? "") : "";
        const model = routed ? String(routed.model ?? "") : "";
        const baseUrl = routed ? String(routed.baseUrl ?? "") : "";
        const apiKey = routed ? String(routed.apiKey ?? "") : "";
        if (routed && kind && kind !== "local" && model) {
          // External provider registered in Settings → Providers: activate it
          // now so the request actually goes out to that provider.
          try {
            await api.configureRemoteModel({ provider: kind, baseUrl, apiKey, model });
            const label = display || kind;
            setModelInfo(model, label);
            rememberLastModel({
              kind: "remote",
              name: model,
              provider: kind,
              providerLabel: label,
              baseUrl,
              apiKey,
            });
            effectiveModel = { name: model, provider: label };
          } catch (err2) {
            addActivity(
              `Could not reach the registered provider: ${String(err2 instanceof Error ? err2.message : err2)}`,
              "error",
            );
            promptModelSelection();
            return;
          }
        } else {
          promptModelSelection();
          addActivity("No model loaded — pick a model in the Models page first.", "error");
          return;
        }
      }

      const fromHome = location.pathname === "/";
      if (fromHome) {
        navigate("/chat");
      }

      setComposerDraft("");
      // A new prompt supersedes any plan awaiting approval.
      discardPlan();
      const turnId = newTurnId();
      setActiveTurnId(turnId);
      // Persist into the chat the user is looking at; a brand-new chat has
      // none yet, so the backend's shared `default` log is the target.
      const chatId = resolveChatId(currentConversationId);
      setActiveChatId(chatId);
      addMessage({ role: "user", content: t, turnId });
      setProcessing(true);
      clearActivity();

      addMessage({ role: "assistant", content: "", streaming: true, model: effectiveModel });

      try {
        const nParams = modelMeta?.nParams;
        const smallModel = Boolean(nParams && !isAgentCapableLocalModel(nParams));
        if (currentMode === "agent" && smallModel) {
          addActivity(
            "This model is very small for Agent mode — use a 3B+ instruct model or a remote provider for reliable edits.",
            "status",
          );
        }
        const prepared = await prepareAgentTurn({
          uiMode: currentMode,
          userText: t,
          projectPath,
          contextPaths,
          contextState,
          activeFile,
          nParams,
          pickWorkspace: () => api.pickWorkspaceFolder(),
        });
        const workspace = prepared.workspace;
        if (workspace && workspace !== projectPath) {
          setProjectPath(workspace);
        }
        // The user turn is now in the manager, so the meter can report the real
        // occupancy. Best-effort: never block a run on the indicator.
        refreshContextUsage();
        const request = buildAgentRequest({
          prompt: t,
          uiMode: currentMode,
          settings,
          maxSteps,
          verify,
          smallModel,
          selectedMode: customMode,
          selectedWorkflow: activeWorkflow,
          images: prepared.images,
        });
        // Plan → Act separation: the backend deliberately refuses to edit
        // while planMode is set, so the plan is shown first and the user
        // explicitly approves before the execute run.
        if (request.planMode) {
          setPendingPlan({ request, turnId, workspace, chatId });
        }
        if (runInBackground) {
          // Detached run: nothing streams back into this bubble, so the
          // placeholder is turned into a pointer to the run timeline.
          request.planMode = false;
          const sessionId = await api.agentRunBackground(request);
          updateLastMessage({
            streaming: false,
            content: `Running in the background (session ${sessionId}). Follow it, and abort it, from the run timeline.`,
          });
          setProcessing(false);
          addActivity(`Started background session ${sessionId}`, "status");
          void appendTurn(
            workspace,
            { role: "user", content: t, ts: Date.now(), turnId },
            chatId,
          );
          return;
        }
        await api.agentRunTask(request);
        void appendTurn(
          workspace,
          { role: "user", content: t, ts: Date.now(), turnId },
          chatId,
        );
      } catch (err) {
        const msg = err instanceof Error ? err.message : String(err);
        addActivity(msg, "error");
        updateLastMessage({ content: (c) => (c || "") + `\n[Error: ${msg}]` });
        setProcessing(false);
        updateLastMessage({ streaming: false });
      }
    },
    [
      location,
      navigate,
      setComposerDraft,
      addMessage,
      setProcessing,
      clearActivity,
      currentMode,
      verify,
      maxSteps,
      projectPath,
      setProjectPath,
      contextPaths,
      modelName,
      modelProvider,
      setModelInfo,
      modelLoading,
      modelRestoreState,
      modelMeta,
      promptModelSelection,
      rememberLastModel,
      addActivity,
      updateLastMessage,
      currentConversationId,
      discardPlan,
      setPendingPlan,
      runInBackground,
      customMode,
      activeWorkflow,
      contextState,
      activeFile,
      settings,
      refreshContextUsage,
    ],
  );

  useEffect(() => {
    const onSendPrompt = (e: Event) => {
      const detail = (e as CustomEvent).detail;
      const text = typeof detail === "string" ? detail : "";
      if (text) void sendPrompt(text);
    };
    window.addEventListener("vencoder-send-prompt", onSendPrompt);
    return () => window.removeEventListener("vencoder-send-prompt", onSendPrompt);
  }, [sendPrompt]);

  return { sendPrompt };
}
