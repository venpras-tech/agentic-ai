import { useCallback, useEffect, useState } from "react";
import { api, type AuditEntry, type PolicyRule, type PolicySnapshot } from "../lib/ipc";
import { useShell, useAgentSettings } from "../context/AppContext";
import "./SecurityPage.css";

type Tab = "policy" | "grants" | "audit";

const POLICY_OPTIONS = ["allow", "ask", "deny"] as const;

/** Auto-approve matrix categories, matching `policy::CATEGORY_KEYS` (backend). */
const AUTO_APPROVE_CATEGORIES: Array<{ key: string; label: string; hint: string }> = [
  { key: "read", label: "Read", hint: "Search, read, list, git read, lints" },
  { key: "edit", label: "Edit", hint: "Write, patch, move/copy/delete files" },
  { key: "shell", label: "Shell & exec", hint: "Terminal commands, scripts, tests" },
  { key: "gitWrite", label: "Git write", hint: "Commit, push, pull, branch, PR" },
  { key: "mcp", label: "MCP", hint: "MCP tool calls and server management" },
  { key: "browser", label: "Web / browser", hint: "Browse, search, extract, download*" },
  { key: "other", label: "Other", hint: "Questions, plans, todos, media" },
];

/** The injected red-zone rule is displayed informatively but is never editable. */
function editableRules(policy: PolicySnapshot | null): PolicyRule[] {
  return (policy?.rules ?? []).filter((r) => r.tool !== "__red_zone__");
}

function SecurityPage() {
  const { projectPath } = useShell();
  const { yolo, setYolo } = useAgentSettings();
  const [tab, setTab] = useState<Tab>("policy");
  const [policy, setPolicy] = useState<PolicySnapshot | null>(null);
  const [audit, setAudit] = useState<AuditEntry[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState("");
  const [draft, setDraft] = useState<{
    default: string;
    rules: PolicyRule[];
    autoApprove: Record<string, boolean>;
  } | null>(null);
  const [saving, setSaving] = useState(false);
  const [saved, setSaved] = useState(false);

  const refresh = useCallback(async () => {
    if (!projectPath) return;
    setLoading(true);
    setError("");
    setSaved(false);
    try {
      // The audit log lives at <workspace>/.ai/audit.jsonl and only exists
      // once the agent has run a tool, so a failure here is expected.
      const [snap, entries] = await Promise.all([
        api.agentPolicySnapshot(),
        api.agentAuditLog(200).catch(() => [] as AuditEntry[]),
      ]);
      setPolicy(snap);
      setAudit(entries ?? []);
      setDraft((prev) =>
        prev ?? { default: snap.default, rules: editableRules(snap), autoApprove: snap.autoApprove ?? {} },
      );
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setLoading(false);
    }
  }, [projectPath]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const revoke = async (path: string) => {
    await api.agentRevokePath(path).catch(() => {});
    await refresh();
  };

  const savePolicy = async () => {
    if (!draft) return;
    setSaving(true);
    setError("");
    setSaved(false);
    try {
      await api.agentPolicySave(draft.default, draft.rules, draft.autoApprove);
      setSaved(true);
      await refresh();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setSaving(false);
    }
  };

  const updateRule = (i: number, patch: Partial<PolicyRule>) => {
    setDraft((prev) => {
      if (!prev) return prev;
      const rules = prev.rules.slice();
      rules[i] = { ...rules[i], ...patch };
      return { ...prev, rules };
    });
  };

  const addRule = () => {
    setDraft((prev) =>
      prev
        ? { ...prev, rules: [...prev.rules, { tool: "", policy: "ask", commandPatterns: [] }] }
        : prev
    );
  };

  const removeRule = (i: number) => {
    setDraft((prev) => {
      if (!prev) return prev;
      const rules = prev.rules.slice();
      rules.splice(i, 1);
      return { ...prev, rules };
    });
  };

  const toggleCategory = (key: string, on: boolean) => {
    setDraft((prev) =>
      prev ? { ...prev, autoApprove: { ...prev.autoApprove, [key]: on } } : prev,
    );
  };

  if (!projectPath) {
    return (
      <div className="security-page">
        <div className="security-empty">Open a project folder to view its policy and audit log.</div>
      </div>
    );
  }

  const grants = policy?.pathGrants ?? [];
  const redZoneVisible = (policy?.rules ?? []).some((r) => r.tool === "__red_zone__");

  return (
    <div className="security-page">
      <header className="security-header">
        <div>
          <h2 className="security-title">Security</h2>
          <p className="security-subtitle">
            Tool policy and audit trail for <code>{projectPath}</code>
          </p>
        </div>
        <div className="security-header-actions">
          <label className="security-yolo">
            <input type="checkbox" checked={yolo} onChange={(e) => setYolo(e.target.checked)} />
            <span>YOLO (auto-approve routine commands)</span>
          </label>
          <button type="button" className="btn-index" onClick={() => void refresh()} disabled={loading}>
            {loading ? "Refreshing…" : "Refresh"}
          </button>
        </div>
      </header>

      <nav className="security-tabs" role="tablist">
        {(["policy", "grants", "audit"] as Tab[]).map((t) => (
          <button
            key={t}
            type="button"
            role="tab"
            aria-selected={tab === t}
            className={`security-tab${tab === t ? " active" : ""}`}
            onClick={() => setTab(t)}
          >
            {t === "policy" ? "Policy" : t === "grants" ? "Path grants" : "Audit log"}
          </button>
        ))}
      </nav>

      {error && <div className="security-error">{error}</div>}

      {tab === "policy" && (
        <section className="security-section">
          <p className="security-note">
            Stored in <code>.ai/policy.json</code>. Default decision for tools that match no rule:
          </p>
          <label className="policy-default-row">
            <span>Default for unmatched tools:</span>
            <select
              value={draft?.default ?? "ask"}
              onChange={(e) => setDraft((prev) => (prev ? { ...prev, default: e.target.value } : prev))}
              disabled={!draft}
            >
              {POLICY_OPTIONS.map((p) => (
                <option key={p} value={p}>
                  {p}
                </option>
              ))}
            </select>
          </label>

          {redZoneVisible && (
            <div className="policy-redzone">
              <strong>Built-in red zone</strong> — destructive commands (rm -rf /, git push --force, …)
              are always denied and cannot be edited here.
            </div>
          )}

          <div className="policy-matrix">
            <div className="policy-matrix-head">
              <strong>Auto-approve by category</strong>
              <span className="policy-matrix-hint">
                Skip the approval dialog for whole tool categories. Red-zone commands, recursive
                folder deletes, and <code>download_file</code> are never auto-approved.
              </span>
            </div>
            <div className="policy-matrix-grid">
              {AUTO_APPROVE_CATEGORIES.map((c) => (
                <label key={c.key} className="policy-matrix-cell" title={c.hint}>
                  <input
                    type="checkbox"
                    checked={Boolean(draft?.autoApprove?.[c.key])}
                    disabled={!draft}
                    onChange={(e) => toggleCategory(c.key, e.target.checked)}
                  />
                  <span className="policy-matrix-label">{c.label}</span>
                  <span className="policy-matrix-desc">{c.hint}</span>
                </label>
              ))}
            </div>
          </div>

          {(draft?.rules ?? []).length === 0 ? (
            <div className="security-empty">No rules — every tool falls through to the default.</div>
          ) : (
            <table className="security-table">
              <thead>
                <tr>
                  <th>Tool</th>
                  <th>Policy</th>
                  <th>Command patterns</th>
                  <th />
                </tr>
              </thead>
              <tbody>
                {(draft?.rules ?? []).map((r, i) => (
                  <tr key={i}>
                    <td>
                      <input
                        className="policy-tool-input"
                        value={r.tool}
                        placeholder="execute_terminal_command"
                        onChange={(e) => updateRule(i, { tool: e.target.value })}
                      />
                    </td>
                    <td>
                      <select
                        value={r.policy}
                        onChange={(e) => updateRule(i, { policy: e.target.value })}
                      >
                        {POLICY_OPTIONS.map((p) => (
                          <option key={p} value={p}>
                            {p}
                          </option>
                        ))}
                      </select>
                    </td>
                    <td>
                      <input
                        className="policy-patterns-input"
                        value={r.commandPatterns.join(",")}
                        placeholder="cargo test, npm test (comma-separated)"
                        onChange={(e) =>
                          updateRule(i, {
                            commandPatterns: e.target.value
                              .split(",")
                              .map((s) => s.trim())
                              .filter(Boolean),
                          })
                        }
                      />
                    </td>
                    <td>
                      <button type="button" className="btn-index" onClick={() => removeRule(i)}>
                        Remove
                      </button>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}

          <div className="policy-actions">
            <button type="button" className="btn-index" onClick={addRule}>
              + Add rule
            </button>
            <button
              type="button"
              className="btn-primary"
              onClick={() => void savePolicy()}
              disabled={saving || !draft}
            >
              {saving ? "Saving…" : "Save policy"}
            </button>
            {saved && <span className="policy-saved">Saved</span>}
          </div>
        </section>
      )}

      {tab === "grants" && (
        <section className="security-section">
          <p className="security-note">
            Session-only allowances for paths outside the workspace. Revoking takes effect on the next
            tool call.
          </p>
          {grants.length === 0 ? (
            <div className="security-empty">No path grants in this session.</div>
          ) : (
            <table className="security-table">
              <thead>
                <tr>
                  <th>Path</th>
                  <th>Mode</th>
                  <th />
                </tr>
              </thead>
              <tbody>
                {grants.map((g) => (
                  <tr key={`${g.path}-${g.mode}`}>
                    <td>
                      <code>{g.path}</code>
                    </td>
                    <td>{g.mode}</td>
                    <td>
                      <button type="button" className="btn-index" onClick={() => void revoke(g.path)}>
                        Revoke
                      </button>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
        </section>
      )}

      {tab === "audit" && (
        <section className="security-section">
          <p className="security-note">
            Newest first, from <code>.ai/audit.jsonl</code>.
          </p>
          {audit.length === 0 ? (
            <div className="security-empty">
              No tool activity recorded yet — run the agent to populate the log.
            </div>
          ) : (
            <table className="security-table">
              <thead>
                <tr>
                  <th>Time</th>
                  <th>Tool</th>
                  <th>Summary</th>
                  <th>Decision</th>
                  <th>Latency</th>
                </tr>
              </thead>
              <tbody>
                {audit.map((e) => (
                  <tr key={e.id} className={e.success === false ? "row-failed" : undefined}>
                    <td className="cell-time">
                      {e.ts ? new Date(e.ts).toLocaleTimeString() : "—"}
                    </td>
                    <td>
                      <code>{e.tool}</code>
                    </td>
                    <td className="cell-summary" title={e.summary}>
                      {e.summary || e.error || "—"}
                    </td>
                    <td className={`decision-${e.decision}`}>{e.decision}</td>
                    <td>{e.latencyMs ? `${e.latencyMs}ms` : "—"}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
        </section>
      )}
    </div>
  );
}

export default SecurityPage;
