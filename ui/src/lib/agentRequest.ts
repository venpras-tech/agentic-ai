import type { AgentTaskRequest, ImageAttachment } from "./ipc";
import { backendAgentMode } from "./agentPrepare";

/** Clamp a possibly-missing/garbage setting into a sane integer range. */
export function num(
  value: unknown,
  fallback: number,
  min: number,
  max: number,
): number {
  const n = typeof value === "number" ? value : Number(value);
  if (!Number.isFinite(n)) return fallback;
  return Math.min(max, Math.max(min, Math.round(n)));
}

function bool(value: unknown, fallback = false): boolean {
  return typeof value === "boolean" ? value : fallback;
}

/** Read a setting under both camelCase and snake_case spellings. */
function pick(settings: Record<string, unknown>, ...keys: string[]): unknown {
  for (const key of keys) {
    if (settings[key] !== undefined) return settings[key];
  }
  return undefined;
}

export interface BuildAgentRequestOptions {
  prompt: string;
  uiMode: string;
  settings: Record<string, unknown>;
  maxSteps: number;
  verify: boolean;
  /** Optional cap on total tokens for the run (0 = no cap). */
  tokenBudget?: number;
  /** Whether the run should search for a goal first, then focus in on it. */
  decompose?: boolean;
  /** True when the local model is too small to drive a long agent loop. */
  smallModel: boolean;
  /** User-defined mode selected in the UI (`.ai/modes/*.md`). */
  selectedMode?: string | null;
  /** User-defined workflow selected in the UI (`.ai/workflows/*.md`). */
  selectedWorkflow?: string | null;
  images?: ImageAttachment[];
  planMode?: boolean;
}

export const DEFAULT_MAX_TOKENS = 4096;
export const DEFAULT_MAX_STEPS = 12;

/**
 * Assemble the `agent_run_task` payload from persisted settings instead of
 * hardcoding it, so the Settings page and the composer cannot drift.
 */
export function buildAgentRequest(opts: BuildAgentRequestOptions): AgentTaskRequest {
  const s = opts.settings ?? {};
  const params = (pick(s, "params") as Record<string, unknown> | undefined) ?? {};

  const maxTokens = num(
    pick(params, "maxTokens", "max_tokens"),
    DEFAULT_MAX_TOKENS,
    64,
    32_768,
  );

  const stepSetting = num(
    pick(params, "maxSteps", "max_steps"),
    opts.maxSteps || DEFAULT_MAX_STEPS,
    1,
    20,
  );
  // Very small local models drift after a few tool rounds; cap them hard.
  const maxSteps = opts.smallModel ? Math.min(stepSetting, 4) : stepSetting;

  // Accept the budget nested under `params` or at the settings top level.
  const tokenBudgetRaw = opts.tokenBudget ?? pick(params, "tokenBudget", "token_budget") ?? pick(s, "tokenBudget", "token_budget");
  const tokenBudget =
    tokenBudgetRaw == null || tokenBudgetRaw === 0
      ? undefined
      : num(tokenBudgetRaw, 0, 0, 10_000_000) || undefined;

  const request: AgentTaskRequest = {
    prompt: opts.prompt,
    maxTokens,
    maxSteps,
    stopWords: [],
    planMode: opts.planMode ?? opts.uiMode === "plan",
    // Verification only makes sense for a mode that is allowed to edit.
    verify: opts.uiMode === "ask" ? false : bool(opts.verify, true),
    decompose: opts.decompose ?? bool(pick(params, "decompose"), false),
    // An explicit user mode wins; otherwise fall back to the built-in ask gate.
    agentMode: opts.selectedMode?.trim() || backendAgentMode(opts.uiMode),
    workflow: opts.selectedWorkflow?.trim() || undefined,
  };

  if (tokenBudget) request.tokenBudget = tokenBudget;
  if (opts.images && opts.images.length > 0) request.images = opts.images;
  return request;
}

/** The follow-up invocation that turns an approved plan into real edits. */
export function buildPlanExecutionRequest(plan: AgentTaskRequest): AgentTaskRequest {
  return { ...plan, planMode: false };
}
