import { useRef, useEffect, useCallback, useMemo, useState } from "react";
import { useNavigate, useLocation } from "react-router-dom";
import { useChat, useAgentSettings, useShell, useContextPane, useModel } from "../context/AppContext";
import ContextSelector from "./ContextSelector";
import AttachmentsPanel from "./AttachmentsPanel";
import { api, type ContextUsage } from "../lib/ipc";
import { isAgentCapableLocalModel } from "../lib/agentPrepare";
import {
  activeMention,
  filterMentionFiles,
  listWorkspaceFiles,
  MENTION_COMMANDS,
} from "../lib/mention";
import { useComposerSend } from "../hooks/useComposerSend";
import { usePlanExecution } from "../hooks/usePlanExecution";
import { useMessageQueue } from "../hooks/useMessageQueue";
import { useAgentStreamContext } from "../context/AgentStreamContext";
import { trackAbort } from "../hooks/useAgentStream";
import { isChatRoute } from "../lib/chatRoute";
import "./BottomBar.css";

function ChatComposer() {
  const {
    composerDraft,
    setComposerDraft,
    currentMode,
    setMode,
    isProcessing,
    addActivity,
    addMessage,
    truncateMessages,
    messages,
    newChatSession,
  } = useChat();
  const { verify, setVerify, yolo, setYolo, maxSteps, tokenBudget, decompose, settings } =
    useAgentSettings();
  const { hitlGate, projectPath } = useShell();
  const { clearContext, addContextPath } = useContextPane();
  const { modelMeta } = useModel();

  const navigate = useNavigate();
  const location = useLocation();
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const inputHistIdxRef = useRef<number | null>(null);
  const [runInBackground, setRunInBackground] = useState(false);
  const [customMode, setCustomMode] = useState<string>("");
  const [activeWorkflow, setActiveWorkflow] = useState<string>("");
  const [modes, setModes] = useState<Array<{ name: string; description: string }>>([]);
  const [workflows, setWorkflows] = useState<Array<{ name: string; description: string }>>([]);

  // Live context-window usage shown next to the input, refreshed after every
  // prepared turn so the meter reflects the pinned buffers plus history.
  const [contextUsage, setContextUsage] = useState<ContextUsage | null>(null);

  // `@` typeahead state: the active token under the caret and its candidates.
  const [mention, setMention] = useState<{ query: string; start: number } | null>(null);
  const [mentionFiles, setMentionFiles] = useState<string[]>([]);
  const [mentionActive, setMentionActive] = useState(0);

  const refreshContextUsage = useCallback(() => {
    void api
      .contextStatus()
      .then(setContextUsage)
      .catch(() => undefined);
  }, []);
  useEffect(() => {
    refreshContextUsage();
  }, [refreshContextUsage]);

  // Load a flat workspace file index once per project for the `@` typeahead.
  useEffect(() => {
    if (!projectPath) {
      setMentionFiles([]);
      return;
    }
    let cancelled = false;
    void listWorkspaceFiles(projectPath).then((files) => {
      if (!cancelled) setMentionFiles(files);
    });
    return () => {
      cancelled = true;
    };
  }, [projectPath]);

  // Candidates for the current mention token: static commands first, then files.
  const mentionCandidates = useMemo(() => {
    if (!mention) return [] as Array<{ kind: "cmd" | "file"; value: string; hint?: string }>;
    const q = mention.query.replace(/^@/, "");
    const cmds = MENTION_COMMANDS.filter((c) => c.token.slice(1).startsWith(q)).map((c) => ({
      kind: "cmd" as const,
      value: c.token,
      hint: c.hint,
    }));
    const files = filterMentionFiles(mentionFiles, q).map((f) => ({
      kind: "file" as const,
      value: `@${f}`,
      hint: undefined,
    }));
    return [...cmds, ...files].slice(0, 12);
  }, [mention, mentionFiles]);

  useEffect(() => {
    setMentionActive(0);
  }, [mention?.query]);

  // Per-category token split for the segmented context bar. Order runs from the
  // least- to the most-evictable, so the bar reads like the eviction pipeline.
  const contextBreakdown = useMemo(() => {
    const b = contextUsage?.breakdown;
    const limit = contextUsage?.limit ?? 0;
    if (!b || limit <= 0) return [];
    const order: Array<[string, string, string]> = [
      ["system", "System prompt", "var(--accent, #0ea5e9)"],
      ["rules", "Project rules", "#8b5cf6"],
      ["memory", "Memory", "#f59e0b"],
      ["skills", "Skills", "#10b981"],
      ["file", "Files", "#06b6d4"],
      ["otherPinned", "Other pinned", "#64748b"],
      ["turns", "History", "#ec4899"],
    ];
    return order
      .map(([key, label, color]) => ({ key, label, color, tokens: Number(b[key] ?? 0) }))
      .filter((s) => s.tokens > 0);
  }, [contextUsage]);

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

  const resetInputHistory = useCallback(() => {
    inputHistIdxRef.current = null;
  }, []);

  const { pendingPlan, setPendingPlan, discardPlan, executePlan } = usePlanExecution({
    refreshContextUsage,
  });

  const { sendPrompt } = useComposerSend({
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
  });

  const { queue, steeringId, enqueueMessage, steerQueued, dequeueMessage, moveQueued, editQueued } =
    useMessageQueue({
    isProcessing,
    sendPrompt,
    onEnqueue: () => {
      setComposerDraft("");
      inputHistIdxRef.current = null;
    },
    onQueueUserTurn: (text, turnId) => {
      addMessage({ role: "user", content: text, turnId });
    },
    addActivity,
  });

  const [editingQueueId, setEditingQueueId] = useState<string | null>(null);
  const [queueDraft, setQueueDraft] = useState("");

  const { trimToast, dismissTrimToast, verifyToast, dismissVerifyToast } = useAgentStreamContext();

  const userInputHistory = useMemo(
    () =>
      messages.filter((m) => m.role === "user" && (m.content || "").trim()).map((m) => m.content),
    [messages],
  );

  const placeholders = useMemo(
    () => ({
      agent: "Ask Vencoder to build, fix, or refactor…",
      ask: "Ask anything about your project…",
      plan: "Describe what you want planned…",
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

  // Append text sent from elsewhere (e.g. a terminal selection via @mention)
  // without clobbering an in-progress draft.
  useEffect(() => {
    const onInsert = (e: Event) => {
      const detail = (e as CustomEvent).detail;
      const text = typeof detail === "string" ? detail : "";
      if (!text) return;
      const joined = composerDraft ? `${composerDraft.replace(/\s+$/, "")}\n${text}` : text;
      setComposerDraft(joined);
      requestAnimationFrame(() => {
        textareaRef.current?.focus();
        textareaRef.current?.setSelectionRange(joined.length, joined.length);
      });
    };
    window.addEventListener("vencoder-insert-composer", onInsert);
    return () => window.removeEventListener("vencoder-insert-composer", onInsert);
  }, [composerDraft, setComposerDraft]);

  useEffect(() => {
    if (!isChatRoute(location.pathname)) return;
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

  const handleCancel = () => {
    trackAbort(api.abortAgentExecution().catch(() => {}));
  };

  const steerCurrentRun = () => {
    const text = composerDraft.trim();
    if (!text) return;
    // Direct steer into the live run (not queued): folded in at the next step
    // boundary by the backend.
    void api.agentSteer(text).catch(() => {});
    setComposerDraft("");
  };

  const handleSlashCommand = (cmd: string) => {
    const c = cmd.toLowerCase();
    if (c === "/new" || c === "/n") {
      newChatSession();
      navigate("/");
      return;
    }
    if (c === "/agents" || c === "/models" || c === "/m") {
      navigate("/agents");
      return;
    }
    if (c === "/index" || c === "/i") {
      navigate("/workspace");
      return;
    }
    if (c === "/clear") {
      clearContext();
      return;
    }
    if (c === "/undo" || c === "/rewind") {
      window.dispatchEvent(new CustomEvent("vencoder-undo-turn"));
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
  const cancelRecordingRef = useRef(false);
  // Push-to-talk: set while the mic button is held so a release that races the
  // async `getUserMedia` handshake still ends the clip instead of hanging.
  const pttHeldRef = useRef(false);

  const startDictation = async () => {
    if (recording || transcribing) return;
    let stream: MediaStream;
    try {
      stream = await navigator.mediaDevices.getUserMedia({ audio: true });
    } catch (err) {
      addActivity(
        `Microphone unavailable — ${err instanceof Error ? err.message : "check permissions"}`,
        "error",
      );
      return;
    }
    const chunks: Blob[] = [];
    const recorder = new MediaRecorder(stream);
    recorderRef.current = recorder;
    cancelRecordingRef.current = false;
    recorder.ondataavailable = (e) => {
      if (e.data.size > 0) chunks.push(e.data);
    };
    recorder.onstop = () => {
      stream.getTracks().forEach((t) => t.stop());
      recorderRef.current = null;
      setRecording(false);
      if (cancelRecordingRef.current) {
        addActivity("Dictation cancelled", "status");
        return;
      }
      setTranscribing(true);
      const blob = new Blob(chunks, { type: recorder.mimeType || "audio/webm" });
      blob
        .arrayBuffer()
        .then((buf) => api.voiceTranscribeData(Array.from(new Uint8Array(buf)), "webm"))
        .then((text) => {
          const t = text.trim();
          if (!t) return;
          setComposerDraft((cur) => (cur ? `${cur} ${t}` : t));
        })
        .catch((err) =>
          addActivity(
            `Transcription failed — ${err instanceof Error ? err.message : String(err)}`,
            "error",
          ),
        )
        .finally(() => setTranscribing(false));
    };
    recorder.start();
    setRecording(true);
    // The pointer was released while we waited for the mic — close the clip now.
    if (!pttHeldRef.current) stopDictation();
  };

  const stopDictation = () => {
    recorderRef.current?.stop();
  };

  const cancelDictation = () => {
    cancelRecordingRef.current = true;
    recorderRef.current?.stop();
  };

  // Escape cancels an in-flight recording without transcribing it.
  useEffect(() => {
    if (!recording) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        cancelRecordingRef.current = true;
        recorderRef.current?.stop();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [recording]);

  const acceptMention = (cand: { kind: "cmd" | "file"; value: string } | undefined) => {
    if (!cand || !mention) return;
    const before = composerDraft.slice(0, mention.start);
    const after = composerDraft.slice(mention.start + 1 + mention.query.length);
    // A file mention pins the path and leaves a trailing space; a static
    // command sends its token through as plain text for the model.
    const insert = cand.kind === "file" ? `${cand.value} ` : `${cand.value} `;
    const next = `${before}${insert}${after}`;
    if (cand.kind === "file") addContextPath(cand.value.slice(1));
    setComposerDraft(next);
    setMention(null);
    requestAnimationFrame(() => {
      const caret = before.length + insert.length;
      textareaRef.current?.focus();
      textareaRef.current?.setSelectionRange(caret, caret);
    });
  };

  const handleDraftChange = (value: string, caret: number) => {
    inputHistIdxRef.current = null;
    setComposerDraft(value);
    setMention(activeMention(value, caret));
  };

  const handleKeyDown = (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
    // `@` typeahead navigation takes priority while the menu is open.
    if (mention && mentionCandidates.length > 0) {
      if (e.key === "ArrowDown") {
        e.preventDefault();
        setMentionActive((i) => (i + 1) % mentionCandidates.length);
        return;
      }
      if (e.key === "ArrowUp") {
        e.preventDefault();
        setMentionActive((i) => (i - 1 + mentionCandidates.length) % mentionCandidates.length);
        return;
      }
      if (e.key === "Enter" || e.key === "Tab") {
        e.preventDefault();
        acceptMention(mentionCandidates[mentionActive]);
        return;
      }
      if (e.key === "Escape") {
        e.preventDefault();
        setMention(null);
        return;
      }
    }
    if (e.key === "Tab" && e.shiftKey) {
      // Cycle permission modes, Claude Code style: Agent → Plan → Ask → Agent.
      e.preventDefault();
      const order = ["agent", "plan", "ask"];
      const i = order.indexOf(currentMode);
      setMode(order[(i + 1) % order.length] ?? "agent");
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
      }
    }
  };

  const showStatusRow = isProcessing;

  const smallModelAgent =
    currentMode === "agent" &&
    modelMeta?.nParams != null &&
    modelMeta.nParams !== 0 &&
    !isAgentCapableLocalModel(modelMeta.nParams);

  const body = (
    <>
      {trimToast && (
        <div className="trim-toast" role="status">
          <span className="trim-toast-text">
            Context trimmed — dropped {trimToast.dropped}, kept {trimToast.remaining} turns
          </span>
          <button
            type="button"
            className="trim-toast-dismiss"
            onClick={dismissTrimToast}
            aria-label="Dismiss"
          >
            ✕
          </button>
        </div>
      )}
      {verifyToast && (
        <div className={`verify-toast ${verifyToast.ok ? "ok" : "error"}`} role="status">
          <span className="verify-toast-text">
            {verifyToast.ok ? "✓ Verify passed" : "⚠ Verify found issues"}
            {verifyToast.command ? ` · ${verifyToast.command}` : ""}
          </span>
          <button
            type="button"
            className="trim-toast-dismiss"
            onClick={dismissVerifyToast}
            aria-label="Dismiss"
          >
            ✕
          </button>
        </div>
      )}
      {showStatusRow ? (
        <div className="status-bar status-bar-top" role="status" aria-live="polite" aria-atomic="true">
          {isProcessing ? <span className="backend-status">Processing…</span> : <span className="backend-status" aria-hidden="true" />}
        </div>
      ) : null}
      <div className="input-row-wrap">
        <form className="input-wrap" onSubmit={(e) => void handleSubmit(e)} aria-busy={isProcessing}>
          <div className={`composer-card ${isProcessing ? "processing" : ""}`}>
            <div className="composer-editor">
              {mention && mentionCandidates.length > 0 && (
                <ul className="mention-menu" role="listbox" aria-label="Context suggestions">
                  {mentionCandidates.map((c, i) => (
                    <li key={`${c.kind}-${c.value}`} role="option" aria-selected={i === mentionActive}>
                      <button
                        type="button"
                        className={`mention-item${i === mentionActive ? " active" : ""}`}
                        onMouseEnter={() => setMentionActive(i)}
                        onMouseDown={(e) => {
                          e.preventDefault();
                          acceptMention(c);
                        }}
                      >
                        <span className="mention-kind">{c.kind === "cmd" ? "⌘" : "📄"}</span>
                        <span className="mention-value">{c.value}</span>
                        {c.hint && <span className="mention-hint">{c.hint}</span>}
                      </button>
                    </li>
                  ))}
                </ul>
              )}
              <textarea
                className="composer-textarea"
                ref={textareaRef}
                value={composerDraft}
                onChange={(e) => {
                  handleDraftChange(e.target.value, e.target.selectionStart ?? e.target.value.length);
                }}
                onKeyDown={handleKeyDown}
                rows={1}
                placeholder={
                  hitlGate
                    ? "Input paused — awaiting approval in the modal above"
                    : isProcessing
                      ? "Agent is working — Enter queues a follow-up, or use Steer to redirect it now"
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
                  {queue.map((m, qi) => (
                    <li key={m.id} className="steer-queue-item">
                      {editingQueueId === m.id ? (
                        <>
                          <input
                            className="steer-queue-edit"
                            value={queueDraft}
                            autoFocus
                            onChange={(e) => setQueueDraft(e.target.value)}
                            onKeyDown={(e) => {
                              if (e.key === "Enter") {
                                e.preventDefault();
                                editQueued(m.id, queueDraft);
                                setEditingQueueId(null);
                              } else if (e.key === "Escape") {
                                e.preventDefault();
                                setEditingQueueId(null);
                              }
                            }}
                          />
                          <span className="steer-queue-actions">
                            <button
                              type="button"
                              className="steer-queue-btn steer-queue-steer"
                              onClick={() => {
                                editQueued(m.id, queueDraft);
                                setEditingQueueId(null);
                              }}
                              title="Save changes"
                            >
                              Save
                            </button>
                          </span>
                        </>
                      ) : (
                        <>
                          <span className="steer-queue-text" title={m.text}>
                            {m.text}
                          </span>
                          <span className="steer-queue-actions">
                            <button
                              type="button"
                              className="steer-queue-btn"
                              onClick={() => moveQueued(m.id, -1)}
                              disabled={qi === 0}
                              title="Move earlier"
                              aria-label="Move queued message earlier"
                            >
                              ↑
                            </button>
                            <button
                              type="button"
                              className="steer-queue-btn"
                              onClick={() => moveQueued(m.id, 1)}
                              disabled={qi === queue.length - 1}
                              title="Move later"
                              aria-label="Move queued message later"
                            >
                              ↓
                            </button>
                            <button
                              type="button"
                              className="steer-queue-btn"
                              onClick={() => {
                                setEditingQueueId(m.id);
                                setQueueDraft(m.text);
                              }}
                              title="Edit this queued message"
                              aria-label="Edit queued message"
                            >
                              ✎
                            </button>
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
                        </>
                      )}
                    </li>
                  ))}
                </ul>
              </div>
            )}
            <ContextSelector />
            <AttachmentsPanel />
            {smallModelAgent && (
              <div className="chat-small-warn" role="status">
                <span className="chat-small-warn-text">
                  Model is ~{(modelMeta!.nParams / 1e9).toFixed(1)}B params — Agent mode
                  sending is disabled for it. Ask/Plan mode or a 3B+ instruct model is safer.
                </span>
                <button
                  type="button"
                  className="chat-small-warn-btn"
                  onClick={() => setMode("ask")}
                >
                  Switch to Ask
                </button>
              </div>
            )}
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
            <div className="composer-toolbar">
              <div className="composer-toolbar-start">
                <button
                  type="button"
                  className="chat-icon-btn chat-icon-attach"
                  onClick={openContextSelector}
                  title="Attach files or context"
                  aria-label="Attach"
                >
                  <span className="chat-icon-btn-label" aria-hidden="true">📎</span>
                </button>
                <button
                  type="button"
                  className={`chat-icon-btn chat-icon-voice${recording ? " recording" : ""}`}
                  onPointerDown={(e) => {
                    if (e.button !== 0) return;
                    e.preventDefault();
                    pttHeldRef.current = true;
                    void startDictation();
                  }}
                  onPointerUp={() => {
                    pttHeldRef.current = false;
                    if (recorderRef.current) stopDictation();
                  }}
                  onPointerLeave={() => {
                    pttHeldRef.current = false;
                    if (recorderRef.current) stopDictation();
                  }}
                  onContextMenu={(e) => {
                    e.preventDefault();
                    if (recorderRef.current) cancelDictation();
                  }}
                  disabled={transcribing}
                  title={
                    transcribing
                      ? "Transcribing…"
                      : recording
                        ? "Release to transcribe · Esc or right-click to cancel"
                        : "Hold to talk (requires local whisper + ffmpeg)"
                  }
                >
                  <span className="chat-icon-btn-label" aria-hidden="true">
                    {recording ? "■" : transcribing ? "…" : "🎤"}
                  </span>
                </button>
                <select
                  className={`composer-mode-select mode-select mode-select-inline${smallModelAgent ? " mode-select-warn" : ""}`}
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
                  onClick={() => navigate("/agents")}
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
                  title="Add context (@)"
                  aria-label="Add context"
                >
                  @
                </button>
              </div>
              <div className="composer-toolbar-end">
                {contextUsage && (
                  <div
                    className={`composer-ctx-meter${contextUsage.overflow ? " overflow" : ""}`}
                    title={[
                      `Context window ${Math.round(contextUsage.usedPercent)}% — ${contextUsage.totalTokens.toLocaleString()} of ${contextUsage.limit.toLocaleString()} tokens (${contextUsage.messageCount} messages)`,
                      ...contextBreakdown.map((s) => `${s.label}: ${s.tokens.toLocaleString()}`),
                    ].join("\n")}
                    role="img"
                    aria-label={`Context window ${Math.round(contextUsage.usedPercent)}% used`}
                  >
                    <div className="composer-ctx-track">
                      {contextBreakdown.length > 0 ? (
                        contextBreakdown.map((s) => (
                          <div
                            key={s.key}
                            className="chat-input-context-seg"
                            style={{
                              width: `${Math.min(100, (s.tokens / contextUsage.limit) * 100)}%`,
                              background: s.color,
                            }}
                          />
                        ))
                      ) : (
                        <div
                          className="chat-input-context-fill"
                          style={{ width: `${Math.min(100, contextUsage.usedPercent)}%` }}
                        />
                      )}
                    </div>
                    <span className="composer-ctx-pct">{Math.round(contextUsage.usedPercent)}%</span>
                  </div>
                )}
                <div className="composer-actions">
                {isProcessing && (
                  <button
                    type="button"
                    className="btn-steer"
                    onClick={steerCurrentRun}
                    disabled={!composerDraft.trim()}
                    title="Send this text into the run that is already in progress"
                  >
                    Steer
                  </button>
                )}
                {isProcessing ? (
                  <button
                    type="button"
                    className="chat-icon-btn btn-cancel"
                    onClick={handleCancel}
                    title="Cancel request"
                    aria-label="Cancel request"
                  >
                    <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
                      <rect x="6" y="6" width="12" height="12" />
                    </svg>
                  </button>
                ) : (
                  <button
                    type="submit"
                    className="chat-icon-btn btn-send"
                    title="Send message"
                    aria-label="Send message"
                    disabled={!composerDraft.trim()}
                  >
                    <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.25">
                      <path d="M12 19V5" />
                      <path d="m5 12 7-7 7 7" />
                    </svg>
                  </button>
                )}
                </div>
              </div>
            </div>
          </div>
        </form>
      </div>
    </>
  );

  return body;
}

export default ChatComposer;