import { useCallback, useEffect, useState } from "react";
import { api, type AuditEntry, type PolicySnapshot } from "../lib/ipc";
import { useApp } from "../context/AppContext";
import "./SecurityPage.css";

type Tab = "policy" | "grants" | "audit";

function SecurityPage() {
  const { projectPath, yolo, setYolo } = useApp();
  const [tab, setTab] = useState<Tab>("policy");
  const [policy, setPolicy] = useState<PolicySnapshot | null>(null);
  const [audit, setAudit] = useState<AuditEntry[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState("");

  const refresh = useCallback(async () => {
    if (!projectPath) return;
    setLoading(true);
    setError("");
    try {
      // The audit log lives at <workspace>/.ai/audit.jsonl and only exists
      // once the agent has run a tool, so a failure here is expected.
      const [snap, entries] = await Promise.all([
        api.agentPolicySnapshot(),
        api.agentAuditLog(200).catch(() => [] as AuditEntry[]),
      ]);
      setPolicy(snap);
      setAudit(entries ?? []);
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

  if (!projectPath) {
    return (
      <div className="security-page">
        <div className="security-empty">Open a project folder to view its policy and audit log.</div>
      </div>
    );
  }

  const grants = policy?.pathGrants ?? [];

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
            Stored in <code>.ai/policy.json</code>. Default decision for tools that match no rule:{" "}
            <strong>{policy?.default ?? "—"}</strong>
          </p>
          {(policy?.rules ?? []).length === 0 ? (
            <div className="security-empty">No rules — every tool falls through to the default.</div>
          ) : (
            <table className="security-table">
              <thead>
                <tr>
                  <th>Pattern</th>
                  <th>Decision</th>
                </tr>
              </thead>
              <tbody>
                {(policy?.rules ?? []).map((r, i) => (
                  <tr key={`${r.pattern}-${i}`}>
                    <td>
                      <code>{r.pattern}</code>
                    </td>
                    <td className={`decision-${r.decision}`}>{r.decision}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
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
