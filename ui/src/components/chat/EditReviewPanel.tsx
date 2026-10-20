import { useRef, useState } from "react";
import DiffViewer from "./DiffViewer";
import { useAgentStreamContext } from "../../context/AgentStreamContext";
import { useShell } from "../../context/AppContext";
import type {
  EditStatus,
  FileEdit,
  OpenEditorTarget,
  PendingEditGroup,
} from "../../lib/ipc";
import "./editreview.css";

const STATUS_LABEL: Record<EditStatus, string> = {
  pending: "pending",
  applied: "applied",
  rejected: "rejected",
  failed: "failed",
  conflict: "conflict",
};

function pathLabel(path: string): string {
  return path.split("/").pop() ?? path;
}

function EditRow({
  edit,
  selected,
  onToggle,
  groupId,
}: {
  edit: FileEdit;
  selected: boolean;
  onToggle: () => void;
  groupId: string;
}) {
  const [open, setOpen] = useState(false);
  const { projectPath } = useShell();
  const { resolveConflict, reviewing } = useAgentStreamContext();

  const rel = (edit.path || "").replaceAll("\\", "/");
  const abs =
    !rel || /^[A-Za-z]:/.test(rel) || rel.startsWith("/")
      ? rel
      : `${(projectPath || "").replace(/[/\\]+$/, "")}/${rel}`;

  const openAtHunk = () => {
    if (!abs) return;
    window.dispatchEvent(
      new CustomEvent<OpenEditorTarget>("vencoder-open-editor", {
        detail: edit.lineStart ? { path: abs, line: edit.lineStart } : abs,
      }),
    );
  };

  return (
    <div className={`edit-row edit-row--${edit.status}`}>
      <label className="edit-row-check">
        <input
          type="checkbox"
          checked={selected}
          disabled={edit.status !== "pending"}
          onChange={onToggle}
        />
        <span className="edit-row-path" title={edit.path}>
          {pathLabel(edit.path)}
        </span>
      </label>
      <span className="edit-row-kind">{edit.kind}</span>
      <span className={`edit-row-status edit-status--${edit.status}`}>
        {STATUS_LABEL[edit.status]}
      </span>
      {edit.error && <span className="edit-row-error" title={edit.error}>⚠</span>}
      <button type="button" className="edit-row-open" onClick={openAtHunk} title="Open this file in the editor">
        open
      </button>
      {edit.diff && (
        <button type="button" className="edit-row-toggle" onClick={() => setOpen((o) => !o)}>
          {open ? "Hide diff" : "Show diff"}
        </button>
      )}
      {edit.status === "conflict" && (
        <span className="edit-row-conflict" title={edit.error}>
          <button
            type="button"
            className="edit-conflict-btn edit-conflict--keep"
            onClick={() => {
              void resolveConflict(groupId, false, [edit.id]);
              openAtHunk();
            }}
            disabled={reviewing}
          >
            Reload from disk
          </button>
          <button
            type="button"
            className="edit-conflict-btn edit-conflict--overwrite"
            onClick={() => void resolveConflict(groupId, true, [edit.id])}
            disabled={reviewing}
          >
            Overwrite
          </button>
        </span>
      )}
      {open && edit.diff && (
        <div className="edit-row-diff">
          <DiffViewer path={edit.path} content={edit.diff} />
        </div>
      )}
    </div>
  );
}

function GroupCard({ group }: { group: PendingEditGroup }) {
  const { applyEditGroup, rejectEditGroup, reviewing, runVerify } = useAgentStreamContext();
  const selectedRef = useRef<Record<string, Set<string>>>({});
  const [, force] = useState(0);

  const pendingIds = group.edits.filter((e) => e.status === "pending").map((e) => e.id);
  const selected = selectedRef.current[group.id];

  const ensure = () => {
    const existing = selectedRef.current[group.id];
    if (!existing) {
      selectedRef.current[group.id] = new Set(pendingIds);
      return selectedRef.current[group.id];
    }
    const pruned = new Set<string>();
    for (const id of existing) if (pendingIds.includes(id)) pruned.add(id);
    selectedRef.current[group.id] = pruned;
    return pruned;
  };

  const chosen = ensure();
  const allSelected = pendingIds.length > 0 && pendingIds.every((id) => chosen.has(id));

  const toggle = (id: string) => {
    if (chosen.has(id)) chosen.delete(id);
    else chosen.add(id);
    force((n) => n + 1);
  };

  const toggleAll = () => {
    if (allSelected) chosen.clear();
    else {
      chosen.clear();
      pendingIds.forEach((id) => chosen.add(id));
    }
    force((n) => n + 1);
  };

  const [reason, setReason] = useState("");

  const apply = () =>
    void applyEditGroup(
      group.id,
      allSelected ? undefined : Array.from(chosen).sort(),
    );
  const reject = () =>
    void rejectEditGroup(
      group.id,
      allSelected ? undefined : Array.from(chosen).sort(),
      reason.trim() || undefined,
    );

  return (
    <section className="edit-group" aria-label="Pending edit review">
      <header className="edit-group-head">
        <label className="edit-group-select">
          <input type="checkbox" checked={allSelected} onChange={toggleAll} />
          <span className="edit-group-title">
            Review changes
            {pendingIds.length > 0 && <span className="edit-group-count">{pendingIds.length}</span>}
          </span>
        </label>
        <span className="edit-group-meta">via {group.tool} · {group.id}</span>
      </header>
      <div className="edit-group-body">
        {group.edits.map((edit) => (
          <EditRow
            key={edit.id}
            edit={edit}
            selected={pendingIds.length > 0 && chosen.has(edit.id)}
            onToggle={() => toggle(edit.id)}
            groupId={group.id}
          />
        ))}
        <div className="edit-group-actions">
          <input
            className="edit-reject-reason"
            value={reason}
            onChange={(e) => setReason(e.target.value)}
            placeholder="Rejection reason to send back (optional)"
            aria-label="Rejection reason"
          />
          <button
            type="button"
            className="edit-action edit-action--reject"
            onClick={reject}
            disabled={reviewing || pendingIds.length === 0}
          >
            Reject
          </button>
          <button
            type="button"
            className="edit-action edit-action--apply"
            onClick={apply}
            disabled={reviewing || pendingIds.length === 0 || chosen.size === 0}
          >
            {reviewing ? "Applying…" : "Apply"}
          </button>
          <button
            type="button"
            className="edit-action"
            onClick={() => void runVerify(group.edits.map((e) => e.path))}
            disabled={reviewing}
            title="Run the workspace verify check (.ai/verify.json or language default)"
          >
            Re-run verify
          </button>
        </div>
      </div>
    </section>
  );
}

function EditReviewPanel() {
  const { pendingEdits } = useAgentStreamContext();
  if (pendingEdits.length === 0) return null;
  return (
    <div className="edit-review-panel" role="group" aria-label="Pending file changes">
      {pendingEdits.map((group) => (
        <GroupCard key={group.id} group={group} />
      ))}
    </div>
  );
}

export default EditReviewPanel;