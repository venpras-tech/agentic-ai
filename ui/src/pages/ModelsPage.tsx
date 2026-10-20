import { useState, useEffect } from "react";
import { useModel } from "../context/AppContext";
import { api } from "../lib/ipc";
import { PROVIDERS, kindByProvider, providerKeyHint, providerUrl } from "../lib/providers";
import DeleteModelDialog, { type DeleteModelTarget } from "../components/DeleteModelDialog";
import "./ModelsPage.css";

function BuiltinIcon() {
  return (
    <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
      <rect x="4" y="4" width="16" height="16" rx="2" />
      <rect x="9" y="9" width="6" height="6" />
      <path d="M9 1v3M15 1v3M9 20v3M15 20v3M1 9h3M1 15h3M20 9h3M20 15h3" />
    </svg>
  );
}

function OllamaIcon() {
  return (
    <svg width="18" height="18" viewBox="0 0 24 24" fill="currentColor">
      <circle cx="12" cy="12" r="10" />
    </svg>
  );
}

function LMStudioIcon() {
  return (
    <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
      <rect x="2" y="3" width="20" height="14" rx="2" />
      <path d="M8 21h8M12 17v4" />
    </svg>
  );
}

function OpenAIIcon() {
  return (
    <svg width="18" height="18" viewBox="0 0 24 24" fill="currentColor">
      <path d="M12 3l1.5 4.5L18 9l-4.5 1.5L12 15l-1.5-4.5L6 9l4.5-1.5L12 3z" />
    </svg>
  );
}

function AnthropicIcon() {
  return (
    <svg width="18" height="18" viewBox="0 0 24 24" fill="currentColor">
      <path d="M17.3041 3.541h-3.6718l6.696 16.918H24Zm-10.6082 0L0 20.459h3.7442l1.3693-3.5527h7.0052l1.3693 3.5528h3.7442L10.5363 3.5409Zm-.3712 10.2232 2.2914-5.9456 2.2914 5.9456Z" />
    </svg>
  );
}

function GoogleIcon() {
  return (
    <svg width="18" height="18" viewBox="0 0 24 24" fill="currentColor">
      <path d="M11.04 19.32Q12 21.51 12 24q0-2.49.93-4.68.96-2.19 2.58-3.81t3.81-2.55Q21.51 12 24 12q-2.49 0-4.68-.93a12.3 12.3 0 0 1-3.81-2.58 12.3 12.3 0 0 1-2.58-3.81Q12 2.49 12 0q0 2.49-.96 4.68-.93 2.19-2.55 3.81a12.3 12.3 0 0 1-3.81 2.58Q2.49 12 0 12q2.49 0 4.68.96 2.19.93 3.81 2.55t2.55 3.81" />
    </svg>
  );
}

function CloudIcon() {
  return (
    <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
      <path d="M17.5 19a4.5 4.5 0 0 0 .5-8.97A6 6 0 0 0 6.2 10.2 4 4 0 0 0 6.5 19Z" />
    </svg>
  );
}

const providerIcons: Record<string, typeof CloudIcon> = {
  local: BuiltinIcon,
  ollama: OllamaIcon,
  lmstudio: LMStudioIcon,
  openai: OpenAIIcon,
  anthropic: AnthropicIcon,
  google: GoogleIcon,
};

const providers = PROVIDERS.map((p) => ({ ...p, icon: providerIcons[p.id] ?? CloudIcon }));

function formatBytes(bytes: number): string {
  if (!bytes) return "—";
  if (bytes >= 1024 ** 3) return `${(bytes / 1024 ** 3).toFixed(1)} GB`;
  if (bytes >= 1024 ** 2) return `${(bytes / 1024 ** 2).toFixed(0)} MB`;
  if (bytes >= 1024) return `${(bytes / 1024).toFixed(0)} KB`;
  return `${bytes} B`;
}

interface LocalModel {
  /** Display label, also the <select> value. */
  name: string;
  path: string;
  sizeBytes: number;
  /** Hub folder (`author/Repo`); `"local"` for a loose file. Delete is keyed on this. */
  repoId: string;
  fileName: string;
}

function ModelsPage() {
  const { modelName, modelProvider, setModelInfo, modelLoadParams, rememberLastModel } = useModel();
  const [selectedProvider, setSelectedProvider] = useState("local");
  const [models, setModels] = useState<LocalModel[]>([]);
  const [selectedModel, setSelectedModel] = useState("");
  const [baseUrlInput, setBaseUrlInput] = useState("");
  const [apiKey, setApiKey] = useState("");
  const [loading, setLoading] = useState(false);
  const [status, setStatus] = useState("");
  const [loadedInfo, setLoadedInfo] = useState<{ name: string; path: string } | null>(null);
  const [connected, setConnected] = useState<{ providerId: string; baseUrl: string } | null>(null);
  const [deleteTarget, setDeleteTarget] = useState<DeleteModelTarget | null>(null);
  const [openingFolder, setOpeningFolder] = useState(false);

  /** Reveal the configured models folder. The backend resolves `modelsDir`,
   *  so this follows the Settings → General override rather than a fixed path. */
  const handleOpenFolder = async () => {
    if (openingFolder) return;
    setOpeningFolder(true);
    try {
      const dir = await api.openModelsDir();
      setStatus(`Opened ${dir}`);
    } catch (err) {
      setStatus(String(err instanceof Error ? err.message : err));
    }
    setOpeningFolder(false);
  };

  const loadModels = async () => {
    setLoading(true);
    setStatus("");
    setModels([]);
    setSelectedModel("");
    try {
      if (selectedProvider === "local") {
        const list = await api.listDownloadedModels();
        setModels(
          list.map((m) => ({
            name: m.fileName || m.repoId,
            path: m.path,
            sizeBytes: m.sizeBytes || 0,
            repoId: m.repoId,
            fileName: m.fileName,
          })),
        );
        if (list.length > 0) {
          setSelectedModel(list[0].fileName || list[0].repoId);
        }
      } else {
        setStatus("Connecting…");
        const result = await api.listRemoteModels({
          provider: kindByProvider[selectedProvider],
          baseUrl: baseUrlInput || providerUrl(selectedProvider),
          apiKey,
        });
        setModels((result || []).map((name) => ({ name, path: "", sizeBytes: 0, repoId: "", fileName: name })));
        if ((result || []).length > 0) setSelectedModel(result[0]);
        setConnected({ providerId: selectedProvider, baseUrl: baseUrlInput || providerUrl(selectedProvider) });
        setStatus(`Connected. ${(result || []).length} model(s) available.`);
      }
    } catch (err) {
      setConnected(null);
      setStatus(String(err instanceof Error ? err.message : err));
    }
    setLoading(false);
  };

  const loadLoadedInfo = async () => {
    try {
      const [info, path] = await Promise.all([api.modelStatus(), api.loadedModelPath()]);
      const name = String((info as { name?: string })?.name || info?.model || "");
      if (name || path) setLoadedInfo({ name, path: path || "" });
      else setLoadedInfo(null);
    } catch {
      setLoadedInfo(null);
    }
  };

  useEffect(() => {
    setBaseUrlInput(providerUrl(selectedProvider));
    setApiKey("");
    setConnected(null);
    setModels([]);
    setSelectedModel("");
    setStatus("");
    if (selectedProvider === "local") {
      void loadModels();
    } else {
      void loadLoadedInfo();
    }
  }, [selectedProvider]);

  const handleLoadDownloaded = async () => {
    if (!selectedModel) {
      setStatus("Select a downloaded model first.");
      return;
    }
    setStatus("Loading model in backend…");
    try {
      const found = models.find((m) => m.name === selectedModel);
      if (!found) throw new Error("Selected model not found on disk.");
      const info = await api.loadModelFromPath(found.path, modelLoadParams());
      const name = String((info as { name?: string })?.name || selectedModel);
      setModelInfo(name, "Local");
      setLoadedInfo({ name, path: found.path });
      rememberLastModel({ kind: "local", name, path: found.path });
      setStatus(`Model loaded: ${name}`);
    } catch (err) {
      setStatus(String(err instanceof Error ? err.message : err));
    }
  };

  const selectedLocal = models.find((m) => m.name === selectedModel) ?? null;
  const selectedIsLoaded = Boolean(selectedLocal && loadedInfo?.path === selectedLocal.path);

  /**
   * Delete the confirmed model, then refresh from the backend rather than
   * patching local state, so what stays on screen matches disk. Selection moves
   * to the next surviving entry because the deleted one is no longer valid.
   */
  const handleDeleteModel = async (target: DeleteModelTarget) => {
    const entry = models.find((m) => m.path === target.path);
    if (!entry) throw new Error("That model is no longer in the list. Refresh and try again.");
    const res = await api.deleteDownloadedModel(entry.repoId, entry.fileName);
    setStatus(
      `Deleted ${entry.fileName} — freed ${formatBytes(res.freedBytes)}.`,
    );
    await loadModels();
    // loadModels clears the selection; fall back to the first row it returns.
    setSelectedModel((prev) => {
      if (prev && prev !== target.fileName) return prev;
      return "";
    });
  };

  const handleSave = async () => {
    setStatus("");
    try {
      if (selectedProvider === "local") {
        await api.pickAndLoadModel(modelLoadParams());
        const path = (await api.loadedModelPath()) || "";
        const name = ((await api.modelStatus()) as { name?: string })?.name || "Model";
        setModelInfo(name, "Local");
        rememberLastModel({ kind: "local", name, path });
      } else {
        await api.configureRemoteModel({
          provider: kindByProvider[selectedProvider],
          baseUrl: baseUrlInput,
          apiKey,
          model: selectedModel,
        });
        setModelInfo(selectedModel || "Model", selectedProvider);
        rememberLastModel({
          kind: "remote",
          name: selectedModel,
          provider: kindByProvider[selectedProvider],
          providerLabel: providers.find((p) => p.id === selectedProvider)?.name || selectedProvider,
          baseUrl: baseUrlInput,
        });
      }
      setStatus("Model configured.");
    } catch (err) {
      setStatus(String(err instanceof Error ? err.message : err));
    }
  };

  return (
    <div className="models-view">
      <div className="models-header">
        <h2 className="models-title">Models</h2>
        <p className="models-desc">Select and manage AI models. Currently: {modelName || "—"} · {modelProvider || "Local"}</p>
      </div>
      <div className="model-modal-body">
        <div className="model-providers-panel">
          <div className="model-providers-panel-title">LLM Provider</div>
          <div className="model-providers-list">
            {providers.map((provider) => {
              const Icon = provider.icon;
              return (
                <button
                  key={provider.id}
                  type="button"
                  className={`model-provider-item ${selectedProvider === provider.id ? "active" : ""}`}
                  onClick={() => setSelectedProvider(provider.id)}
                >
                  <span className="model-provider-icon">
                    <Icon />
                  </span>
                  {provider.name}
                </button>
              );
            })}
          </div>
        </div>
        <div className="model-models-panel">
          <div className="model-models-panel-header">
            <div>
              <div className="model-models-panel-title">Available Models</div>
              <p className="model-models-prompt">
                {selectedProvider !== "local" && !connected
                  ? "Connect to this provider to list the models available to you."
                  : "Select a model to use for this workspace."}
              </p>
            </div>
            <div className="model-models-actions">
              {selectedProvider !== "local" && (
                <button
                  type="button"
                  className="btn-modal-primary model-connect-btn"
                  onClick={() => void loadModels()}
                  disabled={loading}
                >
                  {connected ? "Reconnect" : "Connect"}
                </button>
              )}
              <button
                type="button"
                className="model-refresh-btn"
                onClick={() => void loadModels()}
                title={selectedProvider === "local" ? "Refresh model list" : "Reconnect and reload models"}
              >
              <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
                <path d="M21 12a9 9 0 0 0-9-9 9.75 9.75 0 0 0-6.74 2.74L3 8" />
                <path d="M3 3v5h5" />
                <path d="M3 12a9 9 0 0 0 9 9 9.75 9.75 0 0 0 6.74-2.74L21 16" />
                <path d="M16 21h5v-5" />
              </svg>
            </button>
            </div>
          </div>

          {selectedProvider !== "local" && (
            <>
              <div className="hub-hint">
                <strong>To connect:</strong> enter the endpoint and API key for your account, then press{" "}
                <span className="hub-code">Connect</span> to list the available models. Base URL defaults:
              </div>
              <div className="modal-field model-field-base-url">
                <label className="modal-label" htmlFor="model-base-url">Base URL</label>
                <input
                  type="text"
                  id="model-base-url"
                  className="modal-input"
                  placeholder={providerUrl(selectedProvider) || "https://api.example.com/v1"}
                  value={baseUrlInput}
                  onChange={(e) => setBaseUrlInput(e.target.value)}
                />
              </div>
              {selectedProvider !== "ollama" && (
                <div className="modal-field model-field-api-key">
                  <label className="modal-label" htmlFor="model-api-key">API Key</label>
                  <input
                    type="password"
                    id="model-api-key"
                    className="modal-input"
                    placeholder={providerKeyHint(selectedProvider)}
                    value={apiKey}
                    onChange={(e) => setApiKey(e.target.value)}
                  />
                </div>
              )}
              <p className="models-desc models-hint">
                {selectedProvider === "ollama" && (
                  <>Start an Ollama server (<span className="hub-code">ollama serve</span>) and pull a model first.</>
                )}
                {selectedProvider === "lmstudio" && (
                  <>Start LM Studio's local server, then enable CORS in its "Developer" tab.</>
                )}
              </p>
            </>
          )}

          <div className="modal-field">
            <label className="modal-label" htmlFor="model-select">Model</label>
            {selectedProvider === "local" && !loading && models.length === 0 ? (
              <div className="model-empty-state">
                <svg width="40" height="40" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round">
                  <circle cx="11" cy="11" r="8" />
                  <path d="m21 21-4.3-4.3" />
                  <path d="M8 11h6M11 8v6" />
                </svg>
                <div className="model-empty-title">No local models yet</div>
                <p className="model-empty-sub">
                  Download a GGUF model from the Hugging Face Hub and it will show up here
                  ready to load. Your file system was not scanned — only hub downloads
                  (or files you place in the models folder) are listed.
                </p>
                <button
                  type="button"
                  className="btn-modal-primary"
                  onClick={() => { window.location.hash = "#/models/hub"; }}
                >
                  Browse Hugging Face Hub
                </button>
              </div>
            ) : (
              <div className="modal-select-wrapper">
                <select
                  id="model-select"
                  className="modal-select"
                  value={selectedModel}
                  onChange={(e) => setSelectedModel(e.target.value)}
                >
                  <option value="">Select a model...</option>
                  {models.map((model) => {
                    const isLoaded = loadedInfo?.name === model.name;
                    return (
                      <option key={model.name} value={model.name}>
                        {model.name}{isLoaded ? "  •  loaded" : ""}  ({formatBytes(model.sizeBytes)})
                      </option>
                    );
                  })}
                </select>
              </div>
            )}
          </div>

          {status ? <p className="models-desc">{status}</p> : null}
        </div>
      </div>
      <div className="models-footer">
        {selectedProvider === "local" ? (
          <>
            <a className="btn-modal-hf" href="#/models/hub">
              <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
                <circle cx="12" cy="12" r="10" />
                <path d="M12 16v-4M12 8h.01" />
              </svg>
              Download from Hugging Face
            </a>
            <div className="models-footer-spacer" />
            <button
              type="button"
              className="model-open-folder-btn"
              onClick={() => void handleOpenFolder()}
              disabled={openingFolder}
              title="Open the models folder in your file explorer"
            >
              {openingFolder ? "Opening…" : "Open folder"}
            </button>
            <button
              type="button"
              className="model-delete-btn"
              disabled={!selectedLocal || selectedIsLoaded}
              title={
                selectedIsLoaded
                  ? "This model is currently loaded. Unload it from the title bar first."
                  : selectedLocal
                    ? `Delete ${selectedLocal.fileName} from disk`
                    : "Select a model to delete"
              }
              onClick={() =>
                selectedLocal &&
                setDeleteTarget({
                  fileName: selectedLocal.fileName || selectedLocal.name,
                  path: selectedLocal.path,
                  sizeBytes: selectedLocal.sizeBytes,
                })
              }
            >
              Delete
            </button>
            <button
              type="button"
              className="btn-modal-primary btn-modal-load"
              disabled={!selectedModel}
              onClick={() => void handleLoadDownloaded()}
            >
              Load Model
            </button>
            <button type="button" className="btn-modal-primary" onClick={() => void handleSave()}>
              Browse & Load Model…
            </button>
          </>
        ) : (
          <button
            type="button"
            className="btn-modal-primary"
            disabled={!connected || !selectedModel}
            onClick={() => void handleSave()}
          >
            Save & Load Model
          </button>
        )}
      </div>
      <DeleteModelDialog
        target={deleteTarget}
        onConfirm={handleDeleteModel}
        onClose={() => setDeleteTarget(null)}
      />
    </div>
  );
}

export default ModelsPage;