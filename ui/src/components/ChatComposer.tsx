import { useRef, useEffect, useCallback, useMemo, useState } from "react";
import { useNavigate, useLocation } from "react-router-dom";
import { useApp } from "../context/AppContext";
import { useAgentStream } from "../context/AgentStreamContext";
import ContextSelector from "./ContextSelector";
import { api } from "../lib/ipc";
import { useAgentModesAndWorkflows } from "../hooks/useAgentModesAndWorkflows";
import { usePlanExecution } from "../hooks/usePlanExecution";
import { useMessageQueue } from "../hooks/useMessageQueue";
import { useComposerSend } from "../hooks/useComposerSend";
import "./BottomBar.css";

interface ChatComposerProps {
  variant?: "bottom" | "home";
}

function ChatComposer({ variant = "bottom" }: ChatComposerProps) {
  const {
    composerDraft,
    setComposerDraft,
    currentMode,
    setMode,
    isProcessing,
    verify,
    setVerify,
    yolo,
    setYolo,
    hitlGate,
    addActivity,
    clearContext,
    newChatSession,
    messages,
    truncateMessages,
  } = useApp();
  const { contextUsage } = useAgentStream();
  const { modes, workflows, customMode, setCustomMode, activeWorkflow, setActiveWorkflow } =
    useAgentModesAndWorkflows();
  const { pendingPlan, setPendingPlan, discardPlan, executePlan } = usePlanExecution();
  const [runInBackground, setRunInBackground] = useState(false);
  const { sendPrompt } = useComposerSend({
    customMode,
    activeWorkflow,
    runInBackground,
    discardPlan,
    setPendingPlan,
  });
  const { queue, steeringId, enqueueMessage, steerQueued, dequeueMessage } = useMessageQueue(
    isProcessing,
    sendPrompt,
  );

  const navigate = useNavigate();
  const location = useLocation();
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const inputHistIdxRef = useRef<number | null>(null);

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

  const handleSlashCommand = useCallback(
    (cmd: string) => {
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
    },
    [addActivity, clearContext, navigate, newChatSession, setActiveWorkflow, setMode, workflows],
  );

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

  const handleCancel = () => {
    void api.abortAgentExecution().catch(() => {});
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
  const contextPct =
    contextUsage && contextUsage.limit > 0
      ? Math.min(100, Math.max(0, Math.round(contextUsage.usedPercent)))
      : null;

  const body = (
    <>
      {showStatusRow ? (
        <div className="status-bar status-bar-top" role="status" aria-live="polite" aria-atomic="true">
          {isProcessing ? <span className="backend-status">Processing…</span> : <span className="backend-status" aria-hidden="true" />}
          {contextUsage && (
            <span
              className={`context-meter${contextUsage.overflow ? " overflow" : ""}`}
              title={`${contextUsage.totalTokens} of ${contextUsage.limit} tokens across ${contextUsage.messageCount} messages`}
            >
              Context {contextPct != null ? `${contextPct}%` : `${contextUsage.totalTokens} tok`}
              <span className="context-meter-track">
                <span className="context-meter-fill" style={{ width: `${contextPct ?? 0}%` }} />
              </span>
            </span>
          )}
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
                    discardPlan();
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
