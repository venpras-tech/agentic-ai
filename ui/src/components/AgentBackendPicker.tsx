import { useEffect, useState } from "react";
import { useNavigate } from "react-router-dom";
import { useModel } from "../context/AppContext";
import { isTauriRuntime } from "../lib/ipc";
import { isAgentCapableLocalModel } from "../lib/agentPrepare";
import { useAgentBackends } from "../hooks/useAgentBackends";
import "./AgentBackendPicker.css";

interface AgentBackendPickerProps {
  compact?: boolean;
}

function AgentBackendPicker({ compact }: AgentBackendPickerProps) {
  const navigate = useNavigate();
  const { modelName, modelProvider, modelMeta } = useModel();
  const { backends, activate, activeBackendId, loading } = useAgentBackends();
  const [busy, setBusy] = useState(false);
  const [value, setValue] = useState("local");

  useEffect(() => {
    if (activeBackendId) setValue(activeBackendId);
    else if (modelProvider && modelProvider !== "Local") setValue(`remote:${modelName}`);
    else setValue("local");
  }, [activeBackendId, modelName, modelProvider]);

  const onSelect = async (next: string) => {
    if (next === "local") {
      navigate("/agents?focus=local");
      return;
    }
    if (next === "__add__") {
      navigate("/agents");
      return;
    }
    const row = backends.find((p) => p.id === next);
    if (!row) return;
    setBusy(true);
    try {
      await activate(row);
      setValue(row.id);
    } finally {
      setBusy(false);
    }
  };

  const localWeak =
    modelProvider === "Local" &&
    modelMeta?.nParams != null &&
    !isAgentCapableLocalModel(modelMeta.nParams);

  if (!isTauriRuntime()) return null;

  return (
    <div className={`agent-backend-picker${compact ? " compact" : ""}`}>
      {localWeak && !compact && (
        <p className="agent-backend-hint">
          This local model is best for Ask/chat. Pick a cloud or local-server agent below for full
          Agent mode.
        </p>
      )}
      <label className="agent-backend-label" htmlFor="agent-backend-select">
        Agent backend
      </label>
      <select
        id="agent-backend-select"
        className="agent-backend-select"
        value={value}
        disabled={busy}
        onChange={(e) => void onSelect(e.target.value)}
        title="Which model runs the agent loop"
      >
        <option value="local">
          {modelName && modelProvider === "Local" ? `Local · ${modelName}` : "Local model…"}
        </option>
        {backends.map((p) => (
          <option key={p.id} value={p.id}>
            {p.name} · {p.model}
          </option>
        ))}
        {loading && backends.length === 0 ? (
          <option value="" disabled>Loading agents…</option>
        ) : null}
        <option value="__add__">+ Connect external agent…</option>
      </select>
    </div>
  );
}

export default AgentBackendPicker;
