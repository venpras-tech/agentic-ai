import { useState, useEffect } from "react";
import { useContextPane, useShell } from "../context/AppContext";
import { api, type IndexSearchHit } from "../lib/ipc";
import "./ContextSelector.css";

function ContextSelector() {
  const {
    contextPaths,
    contextState,
    removeContextPath,
    addContextPath,
    setContextState,
  } = useContextPane();
  const { projectPath, indexStatus } = useShell();

  const [showPanel, setShowPanel] = useState(false);
  const [cbQuery, setCbQuery] = useState("");
  const [cbResults, setCbResults] = useState<IndexSearchHit[]>([]);
  const [cbBusy, setCbBusy] = useState(false);

  const runCodebaseSearch = async () => {
    if (!projectPath || !cbQuery.trim()) return;
    setCbBusy(true);
    try {
      const hits = await api.indexSearch(projectPath, cbQuery.trim(), 20);
      setCbResults(hits);
    } catch {
      setCbResults([]);
    } finally {
      setCbBusy(false);
    }
  };

  useEffect(() => {
    const open = () => setShowPanel(true);
    window.addEventListener("vencoder-open-context-selector", open);
    return () => window.removeEventListener("vencoder-open-context-selector", open);
  }, []);

  const hasAnyContext =
    contextPaths.length > 0 ||
    contextState.code.length > 0 ||
    contextState.codebase ||
    contextState.docs.length > 0 ||
    contextState.git ||
    (contextState.web && contextState.web.trim()) ||
    contextState.past_chats ||
    contextState.browser ||
    contextState.visual?.image;

  if (!hasAnyContext && !showPanel) return null;

  return (
    <div className="context-chips-row">
      <div className="context-selector-list">
        {contextPaths.map((path) => (
          <div key={path} className="context-chip" data-type="files">
            <span className="context-chip-label" title={path}>
              {path.split(/[/\\]/).pop()}
            </span>
            <button
              type="button"
              className="context-chip-remove"
              onClick={() => removeContextPath(path)}
              title="Remove from context"
            >
              ×
            </button>
          </div>
        ))}

        {contextState.code.map((seg, i) => (
          <div key={i} className="context-chip" data-type="code">
            <span className="context-chip-label" title={seg.path}>
              {seg.path}
              {seg.startLine ? `:${seg.startLine}-${seg.endLine}` : ""}
            </span>
            <button
              type="button"
              className="context-chip-remove"
              onClick={() => {
                const newCode = [...contextState.code];
                newCode.splice(i, 1);
                setContextState({ code: newCode });
              }}
            >
              ×
            </button>
          </div>
        ))}

        {contextState.codebase && (
          <div className="context-chip context-chip-tag" data-type="codebase">
            <span>@Codebase</span>
            <span className="context-chip-status" title={indexStatus?.lastBuilt ? `Indexed ${new Date(indexStatus.lastBuilt).toLocaleString()}` : undefined}>
              {indexStatus ? `${indexStatus.indexedFiles.toLocaleString()} files` : "not indexed"}
            </span>
            <button
              type="button"
              className="context-chip-remove"
              onClick={() => setContextState({ codebase: false })}
              title="Remove from context"
            >
              ×
            </button>
          </div>
        )}

        {contextState.codebase && projectPath && (
          <div className="context-codebase-search">
            <input
              className="context-codebase-input"
              value={cbQuery}
              placeholder="Search workspace index…"
              onChange={(e) => setCbQuery(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") void runCodebaseSearch();
              }}
            />
            <button
              type="button"
              className="context-codebase-go"
              onClick={() => void runCodebaseSearch()}
              disabled={cbBusy || !cbQuery.trim()}
            >
              {cbBusy ? "…" : "Go"}
            </button>
            {cbResults.length > 0 && (
              <div className="context-codebase-results">
                {cbResults.map((hit, i) => (
                  <button
                    key={i}
                    type="button"
                    className="context-codebase-hit"
                    onClick={() => {
                      addContextPath(hit.path);
                      setCbResults([]);
                      setCbQuery("");
                    }}
                    title={`Add ${hit.path}:${hit.lineNumber} to context`}
                  >
                    {hit.path}:{hit.lineNumber}
                  </button>
                ))}
              </div>
            )}
          </div>
        )}

        {contextState.docs.map((url, i) => (
          <div key={i} className="context-chip" data-type="docs">
            <span className="context-chip-label" title={url}>
              {url}
            </span>
            <button
              type="button"
              className="context-chip-remove"
              onClick={() => {
                const newDocs = [...contextState.docs];
                newDocs.splice(i, 1);
                setContextState({ docs: newDocs });
              }}
            >
              ×
            </button>
          </div>
        ))}

        {contextState.web && (
          <div className="context-chip" data-type="web">
            <span>@Web: {contextState.web}</span>
            <button
              type="button"
              className="context-chip-remove"
              onClick={() => setContextState({ web: "" })}
            >
              ×
            </button>
          </div>
        )}

        {contextState.git && (
          <div className="context-chip context-chip-tag" data-type="git">
            <span>@Git</span>
            <button
              type="button"
              className="context-chip-remove"
              onClick={() => setContextState({ git: null })}
            >
              ×
            </button>
          </div>
        )}

        {contextState.past_chats && (
          <div className="context-chip context-chip-tag" data-type="past_chats">
            <span>@Past Chats</span>
            <button
              type="button"
              className="context-chip-remove"
              onClick={() => setContextState({ past_chats: false })}
            >
              ×
            </button>
          </div>
        )}

        {contextState.browser && (
          <div className="context-chip context-chip-tag" data-type="browser">
            <span>@Browser</span>
            <button
              type="button"
              className="context-chip-remove"
              onClick={() => setContextState({ browser: false })}
            >
              ×
            </button>
          </div>
        )}

        {contextState.visual?.image && (
          <div className="context-chip context-chip-tag" data-type="visual">
            <span>@Image</span>
            <button
              type="button"
              className="context-chip-remove"
              onClick={() => setContextState({ visual: null })}
            >
              ×
            </button>
          </div>
        )}

        {showPanel && !hasAnyContext && (
          <div className="context-quick-add">
            <button type="button" className="context-quick-btn" onClick={() => setContextState({ codebase: true })}>
              @Codebase
            </button>
            <button
              type="button"
              className="context-quick-btn"
              onClick={() => setContextState({ git: { mode: "log" } })}
            >
              @Git
            </button>
            <button type="button" className="context-quick-btn" onClick={() => setContextState({ past_chats: true })}>
              @Past Chats
            </button>
            <button type="button" className="context-quick-btn" onClick={() => setContextState({ browser: true })}>
              @Browser
            </button>
            <button type="button" className="context-quick-dismiss" onClick={() => setShowPanel(false)} title="Dismiss">
              ×
            </button>
          </div>
        )}
      </div>
    </div>
  );
}

export default ContextSelector;