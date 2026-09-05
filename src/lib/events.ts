import type {
  AgentToolEvent,
  BackgroundTaskEvent,
  ExecutionAbortedPayload,
  FileChangedEvent,
  InferenceDone,
  KnowledgeReport,
  ModelInfo,
  PermissionRequest,
  PlanStepEvent,
  QuestionRequest,
  StepEvent,
  SubtaskEvent,
  TodoUpdateEvent,
  ToolOutputEvent,
} from "../types";

export interface StartedEvent {
  sessionId: number;
}
export interface TokenEvent {
  sessionId: number;
  delta: string;
}
export interface DoneEvent {
  sessionId: number;
  done: InferenceDone;
}
export interface ErrorEvent {
  sessionId: number;
  message: string;
}
export interface LoadProgressEvent {
  stage: string;
  progress: number;
}
export interface ToolEvent extends AgentToolEvent {}
export interface AbortedEvent extends ExecutionAbortedPayload {}
export interface PermissionEvent extends PermissionRequest {}
export interface QuestionEvent extends QuestionRequest {}
export interface KnowledgeEvent extends KnowledgeReport {}
export interface FileChangedHandlerEvent extends FileChangedEvent {}
export interface ToolOutputHandlerEvent extends ToolOutputEvent {}
export interface StepHandlerEvent extends StepEvent {}
export interface SubtaskHandlerEvent extends SubtaskEvent {}
export interface PlanStepHandlerEvent extends PlanStepEvent {}
export interface TodoUpdateHandlerEvent extends TodoUpdateEvent {}
export interface BgTaskHandlerEvent extends BackgroundTaskEvent {}
export interface WorkspaceChangedEvent {
  kind: "create" | "modify" | "remove" | "any";
  paths: string[];
}

export const EVT_CONTEXT_TRIMMED = "agent://context-trimmed";

export interface ContextTrimmedEvent {
  sessionId: number;
  dropped: number;
  remaining: number;
}

export interface EngineHandlers {
  onToken: (e: TokenEvent) => void;
  onStarted: (e: StartedEvent) => void;
  onDone: (e: DoneEvent) => void;
  onError: (e: ErrorEvent) => void;
  onModelLoaded: (m: ModelInfo) => void;
  onLoadProgress: (e: LoadProgressEvent) => void;
  onTool?: (e: ToolEvent) => void;
  onAborted?: (e: AbortedEvent) => void;
  onPermission?: (e: PermissionEvent) => void;
  onQuestion?: (e: QuestionEvent) => void;
  onKnowledge?: (e: KnowledgeEvent) => void;
  onFileChanged?: (e: FileChangedHandlerEvent) => void;
  onToolOutput?: (e: ToolOutputHandlerEvent) => void;
  onStep?: (e: StepHandlerEvent) => void;
  onSubtask?: (e: SubtaskHandlerEvent) => void;
  onPlanStep?: (e: PlanStepHandlerEvent) => void;
  onSkillsChanged?: (e: { name: string; path: string }) => void;
  onTodoUpdate?: (e: TodoUpdateHandlerEvent) => void;
  onBgTask?: (e: BgTaskHandlerEvent) => void;
  onWorkspaceChanged?: (e: WorkspaceChangedEvent) => void;
  onContextTrimmed?: (e: ContextTrimmedEvent) => void;
}

export function parseEvent<T>(raw: unknown): T {
  if (typeof raw === "string") {
    try {
      return JSON.parse(raw) as T;
    } catch {
      console.error("[events] Failed to parse event payload:", raw.slice(0, 200));
      return {} as T;
    }
  }
  if (raw && typeof raw === "object") return raw as T;
  console.error("[events] Unexpected event payload type:", typeof raw);
  return {} as T;
}
