import { api, isTauriRuntime, type ImageAttachment } from "./ipc";
import { AGENT_SYSTEM_PROMPT } from "./agentPrompt";
import { buildContext } from "./contextSections";
import type { ActiveFileBuffer, ContextState } from "../context/types";

const SMALL_MODEL_PARAM_CEILING = 2_000_000_000;

export const COMPACT_AGENT_SYSTEM_PROMPT = `You are a coding assistant in the user's workspace. Answer questions in plain text; use tools only when the user asks you to change, run, or inspect something.

Rules:
- Emit tools as <execute_tool>{"type":"tool_name",...}</execute_tool> (one JSON object per block, camelCase fields).
- Never invent tool output. Do not write lines like Response: or pretend a command ran.
- Only act on the user's actual message. Do not delete, push, or overwrite unless they explicitly asked.
- For greetings, questions, or one-word messages, reply briefly in plain text. Ask what they want instead of starting a tool loop.

Flow when action is needed: list_dir / read_file_range / search_file_contents → write_file or apply_file_diff → run_tests or execute_terminal_command.`;

const ASK_MODE_APPEND = `

## Active mode: ask
You are in READ-ONLY mode. Investigate the codebase, answer the user's question, and propose changes — but do NOT modify any files and do NOT run mutating commands. Use read/search tools only when you need workspace evidence; for general questions, answer directly in plain text.`;

export function isAgentCapableLocalModel(nParams?: number): boolean {
  if (nParams == null || nParams === 0) return true;
  return nParams >= SMALL_MODEL_PARAM_CEILING;
}

export function buildSystemPrompt(uiMode: string, nParams?: number): string {
  const base =
    !isAgentCapableLocalModel(nParams) ? COMPACT_AGENT_SYSTEM_PROMPT : AGENT_SYSTEM_PROMPT;
  if (uiMode === "ask") {
    return `${base}${ASK_MODE_APPEND}`;
  }
  return base;
}

export function backendAgentMode(uiMode: string): string | undefined {
  if (uiMode === "ask") return "ask";
  return undefined;
}

export interface PrepareAgentTurnOptions {
  uiMode: string;
  userText: string;
  projectPath: string;
  contextPaths: string[];
  contextState: ContextState;
  activeFile: ActiveFileBuffer | null;
  nParams?: number;
  pickWorkspace: () => Promise<string | null>;
  /** Most recently opened folders — used when no workspace is selected yet. */
  recentProjectPaths?: string[];
}

export interface PreparedTurn {
  workspace: string;
  /** Vision attachments derived from the context chips. */
  images: ImageAttachment[];
}

/**
 * Sync the backend `ContextManager` with everything the agent needs for this
 * turn, then return the resolved workspace root and any image attachments.
 *
 * Order matters: the system prompt and file buffer are *pinned* (never
 * evicted), the user turn is *evictable*, so it is pushed last.
 */
export async function prepareAgentTurn(opts: PrepareAgentTurnOptions): Promise<PreparedTurn> {
  if (!isTauriRuntime()) {
    throw new Error("Agent runs require the desktop app.");
  }
  let workspace = opts.projectPath.trim();
  if (!workspace) {
    const recent = (opts.recentProjectPaths ?? [])
      .map((p) => p.trim())
      .find((p) => p.length > 0);
    if (recent) workspace = recent;
  }
  if (!workspace) {
    workspace = (await opts.pickWorkspace())?.trim() ?? "";
  }
  if (!workspace) {
    throw new Error("Choose a project folder first — the agent needs a workspace to read and edit files.");
  }
  await api.agentSetWorkspace(workspace);

  const context = await buildContext({
    contextPaths: opts.contextPaths,
    contextState: opts.contextState,
    projectPath: workspace,
    activeFile: opts.activeFile,
  });

  const systemPrompt = [buildSystemPrompt(opts.uiMode, opts.nParams), ...context.promptSections]
    .filter(Boolean)
    .join("\n\n");
  await api.contextSetSystemPrompt(systemPrompt);
  await api.contextSetFileBuffer(context.buffer);
  await api.contextPushTurn("user", opts.userText);
  return { workspace, images: context.images };
}
