import { useEffect, useState } from "react";
import { api, isTauriRuntime } from "../lib/ipc";
import { PROVIDER_CATALOG, providerUrl } from "../lib/providers";
import "./ProviderRegistry.css";

type RegistryProvider = {
  id: string;
  name: string;
  kind: string;
  baseUrl?: string;
  apiKey?: string;
  model: string;
  roles?: string[];
  contextSize?: number;
};

type Draft = {
  id: string;
  name: string;
  kind: string;
  baseUrl: string;
  apiKey: string;
  model: string;
  roles: string[];
  contextSize: string;
};

const ROLE_OPTIONS = ["planner", "editor", "autocomplete", "embed"];

const REMOTE_PROVIDERS = PROVIDER_CATALOG.filter((p) => p.id !== "local");

const emptyDraft = (): Draft => ({
  id: "",
  name: "",
  kind: "openai",
  baseUrl: "",
  apiKey: "",
  model: "",
  roles: ["planner", "editor"],
  contextSize: "",
});

function errText(err: unknown): string {
  return String((err as { message?: string })?.message ?? err);
}

function ProviderRegistry() {
  const [providers, setProviders] = useState<RegistryProvider[]>([]);
  const [roleMap, setRoleMap] = useState<Record<string, string>>({});
  const [draft, setDraft] = useState<Draft>(emptyDraft);
  const [editingId, setEditingId] = useState<string | null>(null);
  const [formOpen, setFormOpen] = useState(false);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);
  const [msg, setMsg] = useState<{ text: string; error?: boolean } | null>(null);

  const load = async () => {
    try {
      const list = (await api.providersList()) as unknown as RegistryProvider[];
      setProviders(list);
      const roles: Record<string, string> = {};
      for (const role of ROLE_OPTIONS) {
        const routed = (await api.providersRoute(role)) as unknown as RegistryProvider | null;
        if (routed?.id) roles[role] = routed.id;
      }
      setRoleMap(roles);
    } catch (err) {
      setMsg({ text: errText(err), error: true });
    }
  };

  useEffect(() => {
    if (!isTauriRuntime()) {
      setLoading(false);
      return;
    }
    void load().finally(() => setLoading(false));
  }, []);

  const openAdd = () => {
    setDraft(emptyDraft());
    setEditingId(null);
    setFormOpen(true);
    setMsg(null);
  };

  const openEdit = (p: RegistryProvider) => {
    setDraft({
      id: p.id,
      name: p.name,
      kind: p.kind,
      baseUrl: p.baseUrl ?? "",
      apiKey: p.apiKey ?? "",
      model: p.model,
      roles: p.roles ?? [],
      contextSize: p.contextSize != null ? String(p.contextSize) : "",
    });
    setEditingId(p.id);
    setFormOpen(true);
    setMsg(null);
  };

  const changeKind = (kind: string) => {
    const preset = PROVIDER_CATALOG.find((p) => p.id === kind);
    setDraft((d) => ({
      ...d,
      kind,
      name: d.name || preset?.name || "",
      baseUrl: d.baseUrl || preset?.url || "",
    }));
  };

  const toggleRole = (role: string) => {
    setDraft((d) => ({
      ...d,
      roles: d.roles.includes(role) ? d.roles.filter((r) => r !== role) : [...d.roles, role],
    }));
  };

  const submit = async () => {
    const id = (draft.id.trim() || draft.kind).toLowerCase();
    if (!id) {
      setMsg({ text: "Provider id is required.", error: true });
      return;
    }
    setBusy(true);
    try {
      const payload: RegistryProvider = {
        id,
        name: draft.name.trim() || id,
        kind: draft.kind,
        baseUrl: draft.baseUrl.trim() || undefined,
        apiKey: draft.apiKey.trim() || undefined,
        model: draft.model.trim(),
        roles: draft.roles,
        contextSize: draft.contextSize ? Number(draft.contextSize) : undefined,
      };
      await api.providersUpsert(payload as unknown as Record<string, unknown>);
      setFormOpen(false);
      setMsg({ text: `Provider "${id}" saved.` });
      await load();
    } catch (err) {
      setMsg({ text: errText(err), error: true });
    } finally {
      setBusy(false);
    }
  };

  const remove = async (p: RegistryProvider) => {
    if (!window.confirm(`Remove provider "${p.name}" (${p.id})?`)) return;
    setBusy(true);
    try {
      await api.providersRemove(p.id);
      setMsg({ text: `Provider "${p.id}" removed.` });
      await load();
    } catch (err) {
      setMsg({ text: errText(err), error: true });
    } finally {
      setBusy(false);
    }
  };

  const assignRole = async (role: string, providerId: string) => {
    setBusy(true);
    try {
      if (providerId) await api.providersSetRole(role, providerId);
      else await api.providersClearRole(role);
      setRoleMap((m) => ({ ...m, [role]: providerId }));
      setMsg({ text: `${role} → ${providerId || "none"}.` });
    } catch (err) {
      setMsg({ text: errText(err), error: true });
    } finally {
      setBusy(false);
    }
  };

  return (
    <>
      <h3 className="settings-panel-heading">Providers</h3>
      <p className="settings-hint">
        Register provider endpoints and route agent roles (planner, editor, autocomplete, embed)
        to them. Use <strong>Models</strong> to connect OpenAI, Claude, Ollama, OpenRouter, LiteLLM,
        and other agent backends — they are synced here automatically when you save.
      </p>

      <div className="provider-registry-role-grid">
        {ROLE_OPTIONS.map((role) => (
          <div key={role} className="provider-registry-role-item">
            <label className="settings-label provider-registry-role-label" htmlFor={`role-${role}`}>
              {role}
            </label>
            <select
              id={`role-${role}`}
              className="settings-input"
              value={roleMap[role] ?? ""}
              disabled={busy}
              onChange={(e) => void assignRole(role, e.target.value)}
            >
              <option value="">— none —</option>
              {providers.map((p) => (
                <option key={p.id} value={p.id}>
                  {p.name} ({p.id})
                </option>
              ))}
            </select>
          </div>
        ))}
      </div>

      <div className="settings-mcp-toolbar">
        <button type="button" className="settings-btn settings-btn-primary" onClick={openAdd}>
          <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.2">
            <line x1="12" y1="5" x2="12" y2="19" />
            <line x1="5" y1="12" x2="19" y2="12" />
          </svg>
          Add provider
        </button>
      </div>

      {msg && (
        <p className={`settings-mcp-status-line ${msg.error ? "error" : ""}`}>{msg.text}</p>
      )}

      {formOpen && (
        <div className="settings-mcp-form">
          <div className="settings-mcp-form-grid">
            <div className="settings-field">
              <label className="settings-label" htmlFor="provider-kind">Kind</label>
              <select
                id="provider-kind"
                className="settings-input"
                value={draft.kind}
                onChange={(e) => changeKind(e.target.value)}
              >
                {REMOTE_PROVIDERS.map((p) => (
                  <option key={p.id} value={p.id}>
                    {p.name}
                  </option>
                ))}
              </select>
              <p className="settings-hint">Determines the wire protocol and default endpoint.</p>
            </div>
            <div className="settings-field">
              <label className="settings-label" htmlFor="provider-id">Id</label>
              <input
                id="provider-id"
                className="settings-input"
                placeholder={draft.kind}
                value={draft.id}
                disabled={editingId != null}
                onChange={(e) => setDraft({ ...draft, id: e.target.value })}
              />
              <p className="settings-hint">Unique key used by role routing.</p>
            </div>
            <div className="settings-field">
              <label className="settings-label" htmlFor="provider-name">Name</label>
              <input
                id="provider-name"
                className="settings-input"
                placeholder="Display name"
                value={draft.name}
                onChange={(e) => setDraft({ ...draft, name: e.target.value })}
              />
            </div>
            <div className="settings-field">
              <label className="settings-label" htmlFor="provider-model">Model</label>
              <input
                id="provider-model"
                className="settings-input"
                placeholder="e.g. gpt-4o-mini"
                value={draft.model}
                onChange={(e) => setDraft({ ...draft, model: e.target.value })}
              />
            </div>
            <div className="settings-field settings-mcp-full">
              <label className="settings-label" htmlFor="provider-base-url">Base URL</label>
              <input
                id="provider-base-url"
                className="settings-input"
                placeholder={providerUrl(draft.kind) || "https://api.example.com/v1"}
                value={draft.baseUrl}
                spellCheck={false}
                onChange={(e) => setDraft({ ...draft, baseUrl: e.target.value })}
              />
              <p className="settings-hint">Leave blank to use the provider's default endpoint.</p>
            </div>
            <div className="settings-field">
              <label className="settings-label" htmlFor="provider-api-key">API key</label>
              <input
                id="provider-api-key"
                type="password"
                className="settings-input"
                placeholder="Optional for local servers"
                value={draft.apiKey}
                onChange={(e) => setDraft({ ...draft, apiKey: e.target.value })}
              />
            </div>
            <div className="settings-field">
              <label className="settings-label" htmlFor="provider-context">Context size</label>
              <input
                id="provider-context"
                type="number"
                min="0"
                className="settings-input"
                placeholder="auto"
                value={draft.contextSize}
                onChange={(e) => setDraft({ ...draft, contextSize: e.target.value })}
              />
            </div>
          </div>
          <div className="settings-mcp-check-row">
            {ROLE_OPTIONS.map((role) => (
              <label key={role} className="settings-check provider-registry-role-check">
                <input
                  type="checkbox"
                  checked={draft.roles.includes(role)}
                  onChange={() => toggleRole(role)}
                />
                <span>{role}</span>
              </label>
            ))}
          </div>
          <div className="settings-mcp-form-actions">
            <button type="button" className="settings-btn settings-btn-primary" disabled={busy} onClick={() => void submit()}>
              {editingId != null ? "Update provider" : "Add provider"}
            </button>
            <button type="button" className="settings-btn" onClick={() => setFormOpen(false)}>
              Cancel
            </button>
          </div>
        </div>
      )}

      {loading ? (
        <p className="settings-hint">Loading providers…</p>
      ) : providers.length === 0 && !formOpen ? (
        <div className="settings-mcp-empty">
          <p>No providers registered yet.</p>
          <p className="settings-hint">
            Add a provider to route planner/editor/autocomplete work to a cloud endpoint or a
            local server.
          </p>
        </div>
      ) : (
        <div className="settings-mcp-list">
          {providers.map((p) => (
            <div key={p.id} className="settings-mcp-item">
              <div className="settings-mcp-item-main">
                <div className="settings-mcp-item-title">
                  <span className="settings-mcp-badge on">{p.kind}</span>
                  <span className="settings-mcp-item-name">{p.name}</span>
                  <code className="provider-registry-id">{p.id}</code>
                </div>
                <div className="provider-registry-item-meta">
                  {p.model ? <span>Model: {p.model}</span> : null}
                  {p.baseUrl ? <span title={p.baseUrl}>{p.baseUrl}</span> : null}
                  {p.apiKey ? <span>key set</span> : null}
                </div>
                {(p.roles ?? []).length > 0 && (
                  <div className="provider-registry-item-roles">
                    {(p.roles ?? []).map((role) => (
                      <span key={role} className="provider-registry-role-pill">
                        {role}
                      </span>
                    ))}
                  </div>
                )}
              </div>
              <div className="settings-mcp-item-actions">
                <button type="button" className="settings-btn settings-btn-small" onClick={() => openEdit(p)}>
                  Edit
                </button>
                <button
                  type="button"
                  className="settings-btn settings-btn-small settings-btn-danger"
                  onClick={() => void remove(p)}
                  title="Remove"
                  aria-label={`Remove provider ${p.name}`}
                >
                  <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
                    <circle cx="12" cy="12" r="10" />
                    <line x1="4.93" y1="4.93" x2="19.07" y2="19.07" />
                  </svg>
                </button>
              </div>
            </div>
          ))}
        </div>
      )}
    </>
  );
}

export default ProviderRegistry;
