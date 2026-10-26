import { useCallback, useEffect, useState } from "react";
import { useModel } from "../context/AppContext";
import { api, isTauriRuntime } from "../lib/ipc";
import { catalogKind, providerById } from "../lib/providers";

export type AgentBackend = {
  id: string;
  name: string;
  kind: string;
  baseUrl?: string;
  apiKey?: string;
  model: string;
  roles?: string[];
};

export function useAgentBackends() {
  const { modelName, modelProvider, setModelInfo, rememberLastModel } = useModel();
  const [backends, setBackends] = useState<AgentBackend[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    if (!isTauriRuntime()) {
      setBackends([]);
      return;
    }
    setLoading(true);
    setError(null);
    try {
      const list = (await api.providersList()) as unknown as AgentBackend[];
      setBackends((list ?? []).filter((b) => b.model));
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
      setBackends([]);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void refresh();
    const onChange = () => void refresh();
    window.addEventListener("vencoder-agents-changed", onChange);
    return () => window.removeEventListener("vencoder-agents-changed", onChange);
  }, [refresh]);

  const activate = useCallback(
    async (backend: AgentBackend) => {
      await api.configureRemoteModel({
        provider: backend.kind || catalogKind(backend.id),
        baseUrl: backend.baseUrl ?? "",
        apiKey: backend.apiKey ?? "",
        model: backend.model,
      });
      const label = providerById[backend.id]?.name ?? backend.name;
      setModelInfo(backend.model, label);
      rememberLastModel({
        kind: "remote",
        name: backend.model,
        provider: backend.kind,
        providerLabel: label,
        baseUrl: backend.baseUrl ?? "",
      });
    },
    [rememberLastModel, setModelInfo],
  );

  const activeBackendId =
    modelProvider !== "Local"
      ? backends.find(
          (b) => b.model === modelName || b.name === modelProvider || b.id === modelProvider,
        )?.id ?? null
      : null;

  return {
    backends,
    loading,
    error,
    refresh,
    activate,
    activeBackendId,
    isLocalActive: modelProvider === "Local" || !modelProvider,
    modelName,
    modelProvider,
  };
}
