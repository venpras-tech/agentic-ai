import { useNavigate } from "react-router-dom";
import { useAgentBackends } from "../../hooks/useAgentBackends";
import "./ConnectedAgentsRail.css";

function ConnectedAgentsRail() {
  const navigate = useNavigate();
  const { backends, loading, activate, activeBackendId, isLocalActive, modelName, modelProvider } =
    useAgentBackends();

  return (
    <section className="connected-agents-rail" aria-label="Connected agent backends">
      <div className="connected-agents-head">
        <h2 className="connected-agents-title">Connected agents</h2>
        <button type="button" className="connected-agents-add" onClick={() => navigate("/agents")}>
          + Add agent
        </button>
      </div>
      <div className="connected-agents-cards">
        <button
          type="button"
          className={`agent-card${isLocalActive ? " active" : ""}`}
          onClick={() => navigate("/agents?focus=local")}
        >
          <span className="agent-card-kind">Local</span>
          <span className="agent-card-name">{modelName && isLocalActive ? modelName : "On-device GGUF"}</span>
          <span className="agent-card-meta">Offline inference</span>
        </button>
        {loading && backends.length === 0 ? (
          <div className="agent-card agent-card-muted">Loading agents…</div>
        ) : null}
        {backends.map((b) => (
          <button
            key={b.id}
            type="button"
            className={`agent-card${activeBackendId === b.id ? " active" : ""}`}
            onClick={() => void activate(b)}
          >
            <span className="agent-card-kind">{b.kind}</span>
            <span className="agent-card-name">{b.name}</span>
            <span className="agent-card-meta">{b.model}</span>
          </button>
        ))}
        {backends.length === 0 && !loading ? (
          <button type="button" className="agent-card agent-card-cta" onClick={() => navigate("/agents")}>
            <span className="agent-card-name">Connect OpenAI, Claude, Ollama…</span>
            <span className="agent-card-meta">External agentic backends use the same tool loop</span>
          </button>
        ) : null}
      </div>
      {!isLocalActive && modelName ? (
        <p className="connected-agents-active">
          Active agent: <strong>{modelProvider}</strong> · {modelName}
        </p>
      ) : null}
    </section>
  );
}

export default ConnectedAgentsRail;
