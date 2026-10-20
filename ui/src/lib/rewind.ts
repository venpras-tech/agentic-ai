import { api, type FileCheckpoint } from "./ipc";

/**
 * Pick the checkpoints that rewind a turn. For every file the agent edited at
 * or after `boundaryTs`, the *earliest* pre-apply snapshot is that file's
 * content before the turn, so restoring it once per path rolls the file back
 * cleanly (restoring later snapshots as well would end on a mid-turn version).
 */
export function selectRewindCheckpoints(
  all: FileCheckpoint[],
  boundaryTs: number,
): FileCheckpoint[] {
  const earliest = new Map<string, FileCheckpoint>();
  for (const c of all) {
    if (c.timestampMs < boundaryTs) continue;
    const prev = earliest.get(c.path);
    if (!prev || c.timestampMs < prev.timestampMs) earliest.set(c.path, c);
  }
  return [...earliest.values()];
}

export interface RewindPlan {
  boundaryTs: number;
  checkpoints: FileCheckpoint[];
}

/** Snapshot the checkpoint list once and compute the rewind plan for a turn. */
export async function planRewind(boundaryTs: number): Promise<RewindPlan> {
  const all = await api.checkpointList();
  return { boundaryTs, checkpoints: selectRewindCheckpoints(all, boundaryTs) };
}

/** Restore the planned files; returns how many were written. */
export function restoreRewind(plan: RewindPlan): Promise<number> {
  if (plan.checkpoints.length === 0) return Promise.resolve(0);
  return api.checkpointRestore(plan.checkpoints.map((c) => c.id));
}
