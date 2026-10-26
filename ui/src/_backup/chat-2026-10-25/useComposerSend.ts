import { useCallback } from "react";
import { useApp } from "../context/AppContext";
import { api } from "../lib/ipc";
import { isAgentCapableLocalModel, prepareAgentTurn } from "../lib/agentPrepare";
import { buildAgentRequest } from "../lib/agentRequest";
import { appendTurn, assistantTurnId, newChatId, newTurnId, resolveChatId } from "../lib/sessionLog";
import { claimNextSession, setActiveChatId, setActiveSessionId, setActiveTurnId, awaitPendingAbort, clearActiveSession } from "./useAgentStream";
import type { PendingPlan } from "./usePlanExecution";

export interface SendPromptOptions {
  /** User bubble already shown (e.g. queued while the agent was busy). */
  skipUserBubble?: boolean;
  /** Turn id for the user half when `skipUserBubble` is set. */
  userTurnId?: string;
}

export interface UseComposerSendOptions {
  navigate: (to: string) => void;
  location: { pathname: string };
  setComposerDraft: (text: string) => void;
  resetInputHistory: () => void;
  setPendingPlan: (plan: PendingPlan | null) => void;
  discardPlan: () => void;
  refreshContextUsage: () => void;
  settings: Record<string, unknown>;
  maxSteps: number;
  verify: boolean;
  tokenBudget: number;
  decompose: boolean;
  runInBackground: boolean;
  customMode: string;
  activeWorkflow: string;
}

/** The send path: model-routing check, optimistic bubbles, context prep, and
 *  the run itself (foreground task or detached background session). */
export function useComposerSend(options: UseComposerSendOptions) {
  const {
    navigate,
    location,
    setComposerDraft,
    resetInputHistory,
    setPendingPlan,
    discardPlan,
    refreshContextUsage,
    settings,
    maxSteps,
    verify,
    tokenBudget,
    decompose,
    runInBackground,
    customMode,
    activeWorkflow,
  } = options;

  const {
    addMessage,
    setProcessing,
    clearActivity,
    currentMode,
    projectPath,
    setProjectPath,
    contextPaths,
    contextState,
    activeFile,
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
    setCurrentConversation,
    bumpHistory,
    preferChatComplete,
  } = useApp();

  const sendPrompt = useCallback(
    async (text: string, sendOpts?: SendPromptOptions) => {
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

      // Hard block Agent mode on a too-small local model: these models loop or
      // hallucinate tool calls, so refuse the send instead of warning. Ask and
      // Plan modes (which use no or restricted tools) stay available.
      const nParams = modelMeta?.nParams;
      const smallModel = Boolean(nParams && !isAgentCapableLocalModel(nParams));
      if (currentMode === "agent" && smallModel) {
        addActivity(
          "Agent mode is disabled for this ~" +
            `${((nParams ?? 0) / 1e9).toFixed(1)}B model — switch to Ask or Plan mode, ` +
            "or load a 3B+ instruct model or a remote provider.",
          "error",
        );
        return;
      }

      const fromHome = location.pathname === "/";
      if (fromHome) {
        navigate("/chat");
      }

      setComposerDraft("");
      resetInputHistory();
      // A new prompt supersedes any plan awaiting approval.
      discardPlan();
      const turnId = sendOpts?.userTurnId?.trim() || newTurnId();
      setActiveTurnId(turnId);
      // Persist into the chat the user is looking at. A brand-new session (the
      // app just started, or the user hit "New chat") has no conversation yet,
      // so mint a named chat here: without this the turn would fall through to
      // the backend's shared `default` log and every new chat would pile into
      // the same transcript.
      let chatId = resolveChatId(currentConversationId);
      if (!chatId && !currentConversationId) {
        chatId = newChatId();
        setCurrentConversation(`${projectPath}::${chatId}`);
      }
      setActiveChatId(chatId);
      if (!sendOpts?.skipUserBubble) {
        addMessage({ role: "user", content: t, turnId });
      }
      setProcessing(true);
      clearActivity();

      addMessage({
        role: "assistant",
        content: "",
        streaming: true,
        model: effectiveModel,
        turnId: assistantTurnId(turnId),
      });

      try {
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
        refreshContextUsage();
        const workspace = prepared.workspace;
        if (workspace && workspace !== projectPath) {
          setProjectPath(workspace);
        }
        const request = buildAgentRequest({
          prompt: t,
          uiMode: currentMode,
          settings,
          maxSteps,
          verify,
          tokenBudget,
          decompose,
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
            bumpHistory,
          );
          return;
        }
        // Ask mode can run as a plain chat completion: one turn, zero tool
        // loop (stream_inference attaches the ContextManager snapshot, so the
        // conversation still reaches the model). Custom modes/workflows imply
        // the tool loop, so they keep agent_run_task.
        const useChatComplete =
          currentMode === "ask" && preferChatComplete && !customMode && !activeWorkflow;
        // Claim the events of the run we are about to start before the invoke,
        // so the backend's synchronous `inference-started` (emitted before the
        // command returns) is attributed to this transcript rather than dropped
        // as a foreign session. The returned id then pins the claim.
        // A prior "New chat" abort must reach the backend before we arm this
        // run, or its trigger could cancel the run we are starting.
        await awaitPendingAbort();
        claimNextSession();
        let sessionId: number;
        try {
          sessionId = useChatComplete
            ? await api.streamInference({
                prompt: t,
                maxTokens: request.maxTokens,
                stopWords: request.stopWords ?? [],
                images: prepared.images,
              })
            : await api.agentRunTask(request);
        } catch (invokeErr) {
          // The run never started, so release the claim: leaving it pending
          // would let the next stray engine event adopt the wrong session.
          clearActiveSession();
          throw invokeErr;
        }
        setActiveSessionId(sessionId);
        void appendTurn(
          workspace,
          { role: "user", content: t, ts: Date.now(), turnId },
          chatId,
          bumpHistory,
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
      resetInputHistory,
      discardPlan,
      addMessage,
      setProcessing,
      clearActivity,
      currentMode,
      verify,
      maxSteps,
      projectPath,
      setProjectPath,
      contextPaths,
      contextState,
      activeFile,
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
      setCurrentConversation,
      bumpHistory,
      setPendingPlan,
      runInBackground,
      settings,
      customMode,
      activeWorkflow,
      preferChatComplete,
      refreshContextUsage,
    ],
  );

  return { sendPrompt };
}