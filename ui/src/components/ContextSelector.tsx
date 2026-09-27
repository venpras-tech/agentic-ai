import { useState, useEffect } from "react";
import { useApp } from "../context/AppContext";
import "./ContextSelector.css";

function ContextSelector() {
  const {
    contextPaths,
    contextState,
    removeContextPath,
    setContextState,
  } = useApp();

  const [showPanel, setShowPanel] = useState(false);

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
            <button
              type="button"
              className="context-chip-remove"
              onClick={() => setContextState({ codebase: false })}
            >
              ×
            </button>
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