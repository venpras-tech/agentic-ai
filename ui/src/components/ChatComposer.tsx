import { useRef, useEffect, useCallback, useMemo, useState } from "react";
import { useNavigate, useLocation } from "react-router-dom";
import { listen } from "@tauri-apps/api/event";
import { useApp } from "../context/AppContext";
import ContextSelector from "./ContextSelector";
import { api, type AgentTaskRequest } from "../lib/ipc";
import { isAgentCapableLocalModel, prepareAgentTurn } from "../lib/agentPrepare";
import { buildAgentRequest, buildPlanExecutionRequest } from "../lib/agentRequest";
import { appendTurn, newTurnId, resolveChatId } from "../lib/sessionLog";
import { setActiveChatId, setActiveTurnId } from "../hooks/useAgentStream";
import "./BottomBar.css";

interface ChatComposerProps {
  variant?: "bottom" | "home";
}

/** A plan run that finished and is waiting for the user to approve execution. */
interface PendingPlan {
  request: AgentTaskRequest;
  turnId: string;
  workspace: string;
  chatId: string | null;
}

/** A message typed while the agent was busy, held until the run ends. */
interface QueuedMessage {
  id: string;
  text: string;
}

function ChatComposer({ variant = "bottom" }: ChatComposerProps) {
  const {
    composerDraft,
    setComposerDraft,
    currentMode,
    setMode,
    isProcessing,
    setProcessing,
    verify,
    setVerify,
    yolo,
    setYolo,
    maxSteps,
    hitlGate,
    addMessage,
    updateLastMessage,
    addActivity,
    clearActivity,
    contextPaths,
    projectPath,
    setProjectPath,
    contextState,
    activeFile,
    chatAbortRef,
    messages,
    clearContext,
    newChatSession,
    modelName,
    modelProvider,
    setModelInfo,
    modelLoading,
    modelRestoreState,
    modelMeta,
    promptModelSelection,
    rememberLastModel,
    truncateMessages,
    settings,
    currentConversationId,
  } = useApp();

  const navigate = useNavigate();
  const location = useLocation();
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const inputHistIdxRef = useRef<number | null>(null);
  const [pendingPlan, setPendingPlan] = useState<PendingPlan | null>(null);
  const [runInBackground, setRunInBackground] = useState(false);
  const [customMode, setCustomMode] = useState<string>("");
  const [activeWorkflow, setActiveWorkflow] = useState<string>("");
  const [modes, setModes] = useState<Array<{ name: string; description: string }>>([]);
  const [workflows, setWorkflows] = useState<Array<{ name: string; description: string }>>([]);

  // Load the user-defined modes/workflows so the composer can offer them and
  // pass the selection through to the backend (it gates the allowed tools).
  useEffect(() => {
    let cancelled = false;
    void api
      .modesLoad()
      .then((list) => {
        if (!cancelled) setModes(list ?? []);
      })
      .catch(() => {});
    void api
      .workflowsLoad()
      .then((list) => {
        if (!cancelled) setWorkflows(list ?? []);
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, []);

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
    // of the plan's dedupe window, and the new id is what `useAgentStream`
    // will pair the assistant message with.
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

  const userInputHistory = useMemo(
    () => messages.filter((m) => m.role === "user" && (m.content || "").trim()).map((m) => m.content),
    [messages],
  );

  const placeholders = useMemo(
    () => ({
      agent: "Describe a coding task — the agent will edit files and run commands…",
      ask: "Ask a question or explore the codebase… Enter to send · Shift+Enter new line",
      plan: "Describe the change for a plan (Shift+Tab)… Enter to send · Shift+Enter new line",
    }),
    [],
  );

  useEffect(() => {
    if (textareaRef.current) {
      textareaRef.current.style.height = "auto";
      textareaRef.current.style.height = Math.min(textareaRef.current.scrollHeight, 140) + "px";
    }
  }, [composerDraft]);

  useEffect(() => {
    const onPrefill = (e: Event) => {
      const detail = (e as CustomEvent).detail;
      const text = typeof detail === "string" ? detail : "";
      if (text) {
        setComposerDraft(text);
        requestAnimationFrame(() => {
          textareaRef.current?.focus();
          textareaRef.current?.setSelectionRange(text.length, text.length);
        });
      }
    };
    window.addEventListener("vencoder-prefill-composer", onPrefill);
    return () => window.removeEventListener("vencoder-prefill-composer", onPrefill);
  }, [setComposerDraft]);

  useEffect(() => {
    if (location.pathname !== "/chat") return;
    const p = sessionStorage.getItem("homePrompt");
    if (!p) return;
    sessionStorage.removeItem("homePrompt");
    setComposerDraft(p);
    requestAnimationFrame(() => {
      textareaRef.current?.focus();
      textareaRef.current?.setSelectionRange(p.length, p.length);
    });
  }, [location.pathname, setComposerDraft]);

  useEffect(() => {
    const onFocusComposer = () => {
      requestAnimationFrame(() => {
        const el = textareaRef.current;
        if (el) {
          el.focus();
          const len = el.value.length;
          el.setSelectionRange(len, len);
        }
      });
    };
    window.addEventListener("vencoder-focus-composer", onFocusComposer);
    return () => window.removeEventListener("vencoder-focus-composer", onFocusComposer);
  }, []);

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
      inputHistIdxRef.current = null;
      // A new prompt supersedes any plan awaiting approval.
      discardPlan();
      const turnId = newTurnId();
      setActiveTurnId(turnId);
      // Persist into the chat the user is looking at; a brand-new chat has
      // none yet, so the backend's shared `default` log is the target.
      const chatId = resolveChatId(currentConversationId);
      setActiveChatId(chatId);
      addMessage({ role: "user", content: t });
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
      runInBackground,
    ],
  );

  // ---- steer queue: messages typed while the agent is busy ----------------
  const [queue, setQueue] = useState<QueuedMessage[]>([]);
  const [steeringId, setSteeringId] = useState<string | null>(null);
  // Mirrors `queue` so the drain effect and steer handler can read the current
  // list without re-subscribing on every render.
  const queueRef = useRef<QueuedMessage[]>([]);
  queueRef.current = queue;

  /** Park a message and clear the composer so the user can keep typing. */
  const enqueueMessage = useCallback(
    (text: string) => {
      const item = {
        id: `q-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`,
        text,
      };
      setQueue((q) => [...q, item]);
      setComposerDraft("");
      inputHistIdxRef.current = null;
      addActivity("Queued — the agent is still working on the previous message.", "status");
    },
    [setComposerDraft, addActivity],
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

  useEffect(() => {
    const onEditRequest = (e: Event) => {
      const detail = (e as CustomEvent<{ index?: number; text?: string }>).detail;
      if (typeof detail?.text !== "string") return;
      if (typeof detail.index === "number") truncateMessages(detail.index);
      setComposerDraft(detail.text);
      requestAnimationFrame(() => {
        textareaRef.current?.focus();
        textareaRef.current?.setSelectionRange(detail.text!.length, detail.text!.length);
      });
    };
    window.addEventListener("vencoder-edit-request", onEditRequest);
    return () => window.removeEventListener("vencoder-edit-request", onEditRequest);
  }, [setComposerDraft, truncateMessages]);

  const handleSubmit = async (e?: React.FormEvent) => {
    e?.preventDefault();
    const text = composerDraft.trim();
    if (!text) return;

    if (text.startsWith("/")) {
      handleSlashCommand(text);
      setComposerDraft("");
      return;
    }

    // While a run is in flight, submitting queues instead of starting a second
    // concurrent run. The queue entry can then be steered into the live run, or
    // left to send when the run finishes.
    if (isProcessing) {
      enqueueMessage(text);
      return;
    }

    await sendPrompt(text);
  };

  /** Hold a message for later, or push it into the live run when the user
   *  presses Steer. Backend rejects a steer with no run in progress, which is
   *  the authority on "is anything actually running". */
  const steerQueued = useCallback(
    async (id: string) => {
      const item = queueRef.current.find((q) => q.id === id);
      if (!item || steeringId) return;
      setSteeringId(id);
      try {
        await api.agentSteer(item.text);
        setQueue((q) => q.filter((m) => m.id !== id));
        addActivity("Steering the running agent…", "status");
      } catch (err) {
        // Nothing running (or it just finished): fall back to a normal send so
        // the message is not silently lost.
        addActivity(
          `Could not steer (${err instanceof Error ? err.message : String(err)}) — sending as a new message.`,
          "status",
        );
        setQueue((q) => q.filter((m) => m.id !== id));
        await sendPrompt(item.text);
      }
      setSteeringId(null);
    },
    [steeringId, addActivity, sendPrompt],
  );

  const dequeueMessage = useCallback((id: string) => {
    setQueue((q) => q.filter((m) => m.id !== id));
  }, []);

  /** Drain the queue once the run ends, oldest first. */
  useEffect(() => {
    if (isProcessing) return;
    const pending = queueRef.current;
    if (pending.length === 0) return;
    const [next, ...rest] = pending;
    setQueue(rest);
    void sendPrompt(next.text);
  }, [isProcessing, sendPrompt]);

  // A steer the backend applied lands here; drop the queue row so it is not
  // offered twice.
  useEffect(() => {
    const unlisten = listen<{ id: string }>("agent://steer-applied", (e) => {
      setQueue((q) => q.filter((m) => m.id !== e.payload.id));
    });
    return () => {
      void unlisten.then((fn) => fn());
    };
  }, []);

  const handleCancel = () => {
    void api.abortAgentExecution().catch(() => {});
  };

  const handleSlashCommand = (cmd: string) => {
    const c = cmd.toLowerCase();
    if (c === "/new" || c === "/n") {
      newChatSession();
      navigate("/chat");
      return;
    }
    if (c === "/models" || c === "/m") {
      navigate("/models");
      return;
    }
    if (c === "/index" || c === "/i") {
      navigate("/project");
      return;
    }
    if (c === "/clear") {
      clearContext();
      return;
    }
    const modeMatch = cmd.match(/^\/mode\s+(agent|ask|plan)$/i);
    if (modeMatch) {
      setMode(modeMatch[1].toLowerCase());
      return;
    }
    const workflowMatch = cmd.match(/^\/workflow\s+(\S+)$/i);
    if (workflowMatch) {
      const name = workflowMatch[1];
      if (name.toLowerCase() === "none" || name === "-") {
        setActiveWorkflow("");
        addActivity("Workflow cleared", "status");
      } else if (workflows.some((w) => w.name === name)) {
        setActiveWorkflow(name);
        addActivity(`Workflow: ${name}`, "status");
      } else {
        addActivity(`Unknown workflow: ${name}`, "error");
      }
    }
  };

  const openContextSelector = () => {
    window.dispatchEvent(new CustomEvent("vencoder-open-context-selector"));
  };

  const [recording, setRecording] = useState(false);
  const [transcribing, setTranscribing] = useState(false);
  const recorderRef = useRef<MediaRecorder | null>(null);

  const toggleDictation = async () => {
    if (recording) {
      recorderRef.current?.stop();
      return;
    }
    try {
      const stream = await navigator.mediaDevices.getUserMedia({ audio: true });
      const chunks: Blob[] = [];
      const recorder = new MediaRecorder(stream);
      recorderRef.current = recorder;
      recorder.ondataavailable = (e) => {
        if (e.data.size > 0) chunks.push(e.data);
      };
      recorder.onstop = () => {
        stream.getTracks().forEach((t) => t.stop());
        setRecording(false);
        setTranscribing(true);
        const blob = new Blob(chunks, { type: recorder.mimeType });
        blob
          .arrayBuffer()
          .then((buf) =>
            api.voiceTranscribeData(
              Array.from(new Uint8Array(buf)),
              "webm",
            ),
          )
          .then((text) => {
            const t = text.trim();
            if (!t) return;
            setComposerDraft((cur) => (cur ? `${cur} ${t}` : t));
          })
          .catch(() => {})
          .finally(() => setTranscribing(false));
      };
      recorder.start();
      setRecording(true);
    } catch {
      // mic permission denied / unsupported — stay silent
    }
  };

  const handleKeyDown = (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
    if (e.key === "Tab" && e.shiftKey) {
      e.preventDefault();
      setMode("plan");
      return;
    }
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      inputHistIdxRef.current = null;
      void handleSubmit();
      return;
    }
    if ((e.ctrlKey || e.metaKey) && (e.key === "ArrowUp" || e.key === "ArrowDown")) {
      e.preventDefault();
      const uh = userInputHistory;
      if (uh.length === 0) return;
      let idx = inputHistIdxRef.current;
      if (e.key === "ArrowUp") {
        if (idx === null) idx = uh.length;
        idx = Math.max(0, idx - 1);
      } else {
        if (idx === null) idx = -1;
        idx = Math.min(uh.length, idx + 1);
      }
      inputHistIdxRef.current = idx;
      if (idx >= 0 && idx < uh.length) {
        setComposerDraft(uh[idx]);
      } else {
        setComposerDraft("");
        inputHistIdxRef.current = uh.length;
      }
      return;
    }
    if (e.key === "@" || (e.key === "2" && e.shiftKey)) {
      openContextSelector();
    }
  };

  const showStatusRow = variant === "bottom" || isProcessing;

  const body = (
    <>
      {showStatusRow ? (
        <div className="status-bar status-bar-top" role="status" aria-live="polite" aria-atomic="true">
          {isProcessing ? <span className="backend-status">Processing…</span> : <span className="backend-status" aria-hidden="true" />}
        </div>
      ) : null}
      <div className="input-row-wrap">
        <form className="input-wrap" onSubmit={(e) => void handleSubmit(e)} aria-busy={isProcessing}>
          <div className={`chat-input-box ${isProcessing ? "processing" : ""}`}>
            <div className="chat-input-main">
              <textarea
                ref={textareaRef}
                value={composerDraft}
                onChange={(e) => {
                  inputHistIdxRef.current = null;
                  setComposerDraft(e.target.value);
                }}
                onKeyDown={handleKeyDown}
                rows={1}
                placeholder={
                  hitlGate
                    ? "Input paused — awaiting approval in the modal above"
                    : isProcessing
                      ? "Agent is working — Enter queues a follow-up, or steer the current run"
                      : placeholders[currentMode as keyof typeof placeholders] || placeholders.agent
                }
                aria-label="Chat message"
                disabled={Boolean(hitlGate)}
              />
            </div>
            {queue.length > 0 && (
              <div className="steer-queue" role="group" aria-label="Queued messages">
                <div className="steer-queue-title">
                  Queued ({queue.length}) — steers the run in progress
                </div>
                <ul className="steer-queue-list">
                  {queue.map((m) => (
                    <li key={m.id} className="steer-queue-item">
                      <span className="steer-queue-text" title={m.text}>
                        {m.text}
                      </span>
                      <span className="steer-queue-actions">
                        <button
                          type="button"
                          className="steer-queue-btn steer-queue-steer"
                          onClick={() => void steerQueued(m.id)}
                          disabled={steeringId === m.id}
                          title="Inject this message into the agent's current run"
                        >
                          {steeringId === m.id ? "Steering…" : "Steer"}
                        </button>
                        <button
                          type="button"
                          className="steer-queue-btn"
                          onClick={() => dequeueMessage(m.id)}
                          title="Remove from the queue"
                          aria-label={`Remove queued message: ${m.text.slice(0, 40)}`}
                        >
                          ✕
                        </button>
                      </span>
                    </li>
                  ))}
                </ul>
              </div>
            )}
            <ContextSelector />
            {pendingPlan && !isProcessing && (
              <div className="plan-approval" role="group" aria-label="Plan approval">
                <span className="plan-approval-text">Plan ready — review it, then execute?</span>
                <button
                  type="button"
                  className="plan-approval-btn plan-approval-exec"
                  onClick={() => void executePlan()}
                >
                  Execute plan
                </button>
                <button
                  type="button"
                  className="plan-approval-btn"
                  onClick={() => {
                    setPendingPlan(null);
                    addActivity("Plan discarded", "status");
                  }}
                >
                  Discard
                </button>
              </div>
            )}
            <div className="chat-input-footer">
              <div className="chat-input-icons">
                <button
                  type="button"
                  className="chat-icon-btn chat-icon-attach"
                  onClick={openContextSelector}
                  title="Attach files or context to this chat"
                >
                  <span className="chat-icon-btn-label" aria-hidden="true">📎</span>
                  <span className="chat-icon-btn-text">Attach</span>
                </button>
                <button
                  type="button"
                  className={`chat-icon-btn chat-icon-voice${recording ? " recording" : ""}`}
                  onClick={() => void toggleDictation()}
                  disabled={transcribing}
                  title={
                    transcribing
                      ? "Transcribing…"
                      : recording
                        ? "Stop recording and transcribe"
                        : "Dictate (requires local whisper + ffmpeg)"
                  }
                >
                  <span className="chat-icon-btn-label" aria-hidden="true">
                    {recording ? "■" : transcribing ? "…" : "🎤"}
                  </span>
                  <span className="chat-icon-btn-text">
                    {recording ? "Stop" : transcribing ? "…" : "Voice"}
                  </span>
                </button>
                <select
                  className="mode-select mode-select-inline"
                  value={currentMode}
                  onChange={(e) => setMode(e.target.value)}
                  title="Agent: implement changes. Ask: explore only. Plan: create Markdown plan"
                >
                  <option value="agent">Agent</option>
                  <option value="ask">Ask</option>
                  <option value="plan">Plan</option>
                </select>
                {currentMode === "agent" && (
                  <button
                    type="button"
                    className={`chat-icon-btn chat-icon-verify${verify ? " active" : ""}`}
                    onClick={() => setVerify(!verify)}
                    title="After edits, run tests/typecheck before finishing"
                  >
                    <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
                      <path d="M9 11l3 3L22 4" />
                      <path d="M21 12v7a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h11" />
                    </svg>
                  </button>
                )}
                {currentMode === "agent" && (
                  <button
                    type="button"
                    className={`chat-icon-btn chat-icon-yolo${yolo ? " active yolo-active" : ""}`}
                    onClick={() => setYolo(!yolo)}
                    title="YOLO: auto-approve routine shell commands (tests/builds/inspects). Red-zone stays blocked."
                  >
                    <span className="chat-icon-btn-text yolo-word">YOLO</span>
                  </button>
                )}
                {currentMode === "agent" && (
                  <button
                    type="button"
                    className={`chat-icon-btn chat-icon-bg${runInBackground ? " active" : ""}`}
                    onClick={() => setRunInBackground(!runInBackground)}
                    title="Run detached: the agent keeps working after you navigate away. Track and abort it from the run timeline."
                  >
                    <span className="chat-icon-btn-text">BG</span>
                  </button>
                )}
                {modes.length > 0 && (
                  <select
                    className="mode-select mode-select-inline"
                    value={customMode}
                    onChange={(e) => setCustomMode(e.target.value)}
                    title="User-defined agent mode (.ai/modes/*.md) — restricts which tools may run"
                  >
                    <option value="">Mode: default</option>
                    {modes.map((m) => (
                      <option key={m.name} value={m.name} title={m.description}>
                        {m.name}
                      </option>
                    ))}
                  </select>
                )}
                {workflows.length > 0 && (
                  <select
                    className="mode-select mode-select-inline"
                    value={activeWorkflow}
                    onChange={(e) => setActiveWorkflow(e.target.value)}
                    title="User-defined workflow (.ai/workflows/*.md) — gates the allowed tools"
                  >
                    <option value="">Workflow: none</option>
                    {workflows.map((w) => (
                      <option key={w.name} value={w.name} title={w.description}>
                        {w.name}
                      </option>
                    ))}
                  </select>
                )}
                <button
                  type="button"
                  className="chat-icon-btn chat-icon-model"
                  title="Models – choose AI model"
                  onClick={() => navigate("/models")}
                >
                  <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
                    <path d="M9.5 2A2.5 2.5 0 0 1 12 4.5v15a2.5 2.5 0 0 1-4.96.44 2.5 2.5 0 0 1-2.96-3.08 3 3 0 0 1-.34-5.58 2.5 2.5 0 0 1 1.32-4.24 2.5 2.5 0 0 1 1.98-3A2.5 2.5 0 0 1 9.5 2Z" />
                    <path d="M14.5 2A2.5 2.5 0 0 0 12 4.5v15a2.5 2.5 0 0 0 4.96.44 2.5 2.5 0 0 0 2.96-3.08 3 3 0 0 0 .34-5.58 2.5 2.5 0 0 0-1.32-4.24 2.5 2.5 0 0 0-1.98-3A2.5 2.5 0 0 0 14.5 2Z" />
                  </svg>
                </button>
                <button
                  type="button"
                  className="chat-icon-btn chat-icon-at"
                  onClick={openContextSelector}
                  title="Context – Add more context"
                >
                  @
                </button>
              </div>
              <div className="chat-input-actions">
                {isProcessing ? (
                  <button
                    type="button"
                    className="chat-icon-btn btn-cancel"
                    onClick={handleCancel}
                    title="Cancel request"
                  >
                    <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
                      <rect x="6" y="6" width="12" height="12" />
                    </svg>
                  </button>
                ) : (
                  <button type="submit" className="chat-icon-btn btn-send" title="Send">
                    <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
                      <path d="M22 2L11 13" />
                      <path d="M22 2L15 22L11 13L2 9L22 2Z" />
                    </svg>
                  </button>
                )}
              </div>
            </div>
          </div>
        </form>
      </div>
    </>
  );

  if (variant === "home") {
    return <div className="chat-composer chat-composer--home">{body}</div>;
  }

  return body;
}

export default ChatComposer;