import { useState, useEffect, useCallback, useMemo } from "react";
import { useChat, useShell } from "../context/AppContext";
import { api } from "../lib/ipc";
import { isDefaultChat, newChatId, splitConversationId } from "../lib/sessionLog";
import "./HistoryPanel.css";

function HistoryPanel() {
  const {
    conversations,
    setConversations,
    currentConversationId,
    setCurrentConversation,
    loadConversation,
    loadingConversation,
    historyVersion,
  } = useChat();
  const { historyPanelOpen, setHistoryPanelOpen } = useShell();
  const [selectedIds, setSelectedIds] = useState<Set<string>>(new Set());
  const [busy, setBusy] = useState(false);

  // The backend refuses to fork or delete the base chat, and addressing it as
  // `sessions/<key>/default.jsonl` would silently hit a file that does not
  // exist. Reflect that in the UI instead of offering an action that quietly
  // does nothing.
  // Compare by chat id only: a freshly minted conversation is addressed with
  // its raw project path, while the history row carries the backend's
  // sanitized project key, so a full-id compare would never match.
  const activeChatId = useMemo(
    () => (currentConversationId ? splitConversationId(currentConversationId).chatId : null),
    [currentConversationId],
  );

  const selectedCanMutate = useMemo(
    () =>
      Array.from(selectedIds).length > 0 &&
      Array.from(selectedIds).every((id) => !isDefaultChat(id)),
    [selectedIds],
  );

  const fetchHistory = useCallback(async () => {
    try {
      const projects = await api.sessionProjects();
      const chats: Array<{ id: string; title: string; created_at: string }> = [];
      for (const proj of projects) {
        const list = proj.chats || [];
        for (const c of list) {
          chats.push({
            id: `${proj.key}::${c.id || "default"}`,
            title: c.title || "New chat",
            created_at: new Date(c.updatedAtMs || Date.now()).toISOString(),
          });
        }
      }
      chats.sort((a, b) => new Date(b.created_at).getTime() - new Date(a.created_at).getTime());
      setConversations(chats.slice(0, 100));
    } catch {
      console.error("Failed to fetch history");
    }
  }, [setConversations]);

  // Refetch when the panel opens and whenever a turn is persisted. Without the
  // second trigger a chat saved during this session stays invisible until the
  // panel is closed and reopened.
  useEffect(() => {
    if (historyPanelOpen) fetchHistory();
  }, [historyPanelOpen, historyVersion, fetchHistory]);

  const handleSelectAll = (e: React.ChangeEvent<HTMLInputElement>) => {
    if (e.target.checked) {
      setSelectedIds(new Set(conversations.map((c) => c.id)));
    } else {
      setSelectedIds(new Set());
    }
  };

  const handleSelect = (id: string) => {
    const newSet = new Set(selectedIds);
    if (newSet.has(id)) {
      newSet.delete(id);
    } else {
      newSet.add(id);
    }
    setSelectedIds(newSet);
  };

  const handleConversationClick = async (id: string) => {
    setCurrentConversation(id);
    setHistoryPanelOpen(false);
    await loadConversation(id);
  };

  /** Branch the selected chat into a new one, copying its transcript. */
  const handleFork = async () => {
    const id = Array.from(selectedIds)[0];
    if (!id || busy || isDefaultChat(id)) return;
    setBusy(true);
    try {
      const { project, chatId } = splitConversationId(id);
      const target = newChatId();
      await api.sessionFork(project, chatId, null, target);
      setSelectedIds(new Set());
      await fetchHistory();
    } catch (err) {
      console.error("Failed to fork conversation", err);
    } finally {
      setBusy(false);
    }
  };

  const handleDelete = async () => {
    const ids = Array.from(selectedIds).filter((id) => !isDefaultChat(id));
    if (ids.length === 0) {
      setSelectedIds(new Set());
      return;
    }
    for (const id of ids) {
      try {
        const { project, chatId } = splitConversationId(id);
        await api.sessionDeleteChat(project, chatId);
      } catch {
        console.error("Failed to delete conversation", id);
      }
    }
    setConversations(conversations.filter((c) => !selectedIds.has(c.id)));
    setSelectedIds(new Set());
  };

  const formatDate = (iso: string) => {
    const d = new Date(iso);
    const now = new Date();
    const today = new Date(now.getFullYear(), now.getMonth(), now.getDate());
    const yesterday = new Date(today);
    yesterday.setDate(yesterday.getDate() - 1);
    const dDate = new Date(d.getFullYear(), d.getMonth(), d.getDate());

    if (dDate.getTime() === today.getTime()) return "Today";
    if (dDate.getTime() === yesterday.getTime()) return "Yesterday";
    return d.toLocaleDateString([], { month: "short", day: "numeric", year: "numeric" });
  };

  const groupedConversations = conversations.reduce<Record<string, Array<typeof conversations[0]>>>((acc, conv) => {
    const key = formatDate(conv.created_at);
    if (!acc[key]) acc[key] = [];
    acc[key].push(conv);
    return acc;
  }, {});

  const groupOrder = ["Today", "Yesterday"];
  const sortedKeys = Object.keys(groupedConversations).sort((a, b) => {
    const ai = groupOrder.indexOf(a);
    const bi = groupOrder.indexOf(b);
    if (ai !== -1 && bi !== -1) return ai - bi;
    if (ai !== -1) return -1;
    if (bi !== -1) return 1;
    return new Date(groupedConversations[b][0].created_at).getTime() - new Date(groupedConversations[a][0].created_at).getTime();
  });

  return (
    <div className="history-panel-wrap" hidden={!historyPanelOpen}>
      <aside className="history-panel">
        <div className="history-panel-header">
          <h3 className="history-panel-title">Chat history</h3>
          <div className="history-panel-actions">
            <label className="history-check-wrap">
              <input
                type="checkbox"
                className="history-select-all"
                checked={conversations.length > 0 && selectedIds.size === conversations.length}
                onChange={handleSelectAll}
              />
              <span className="history-check-label">Select</span>
            </label>
            <button
              type="button"
              className="btn-history-delete"
              onClick={() => void handleFork()}
              disabled={selectedIds.size !== 1 || busy || !selectedCanMutate}
              title={
                selectedCanMutate
                  ? "Branch this chat into a new one"
                  : "The base chat cannot be forked"
              }
            >
              Fork
            </button>
            <button
              type="button"
              className="btn-history-delete"
              onClick={() => void handleDelete()}
              disabled={selectedIds.size === 0 || !selectedCanMutate}
              title={selectedCanMutate ? undefined : "The base chat cannot be deleted"}
            >
              Delete
            </button>
          </div>
        </div>
        <div className="history-panel-list">
          {loadingConversation ? (
            <div className="history-empty">Loading conversation…</div>
          ) : sortedKeys.length === 0 ? (
            <div className="history-empty">No conversations yet</div>
          ) : (
            sortedKeys.map((key) => (
              <div key={key} className="history-group">
                <div className="history-group-label">{key}</div>
                {groupedConversations[key].map((conv) => (
                  <label
                    key={conv.id}
                    className={`history-item-wrap ${activeChatId && splitConversationId(conv.id).chatId === activeChatId ? "active" : ""}`}
                  >
                    <input
                      type="checkbox"
                      className="history-item-cb"
                      checked={selectedIds.has(conv.id)}
                      onChange={() => handleSelect(conv.id)}
                    />
                    <span
                      className="history-item-text"
                      role="button"
                      tabIndex={0}
                      onClick={() => handleConversationClick(conv.id)}
                      onKeyDown={(e) => {
                        if (e.key === "Enter" || e.key === " ") {
                          e.preventDefault();
                          handleConversationClick(conv.id);
                        }
                      }}
                    >
                      {conv.title || "New chat"}
                    </span>
                  </label>
                ))}
              </div>
            ))
          )}
        </div>
      </aside>
      <div
        className="history-resize-handle"
        title="Drag to resize"
        aria-hidden="true"
      />
    </div>
  );
}

export default HistoryPanel;