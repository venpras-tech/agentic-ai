import type {
  AgentToolEvent,
  ExecutionAbortedPayload,
  FileChangedEvent,
  InferenceDone,
  KnowledgeReport,
  ModelInfo,
  PermissionRequest,
  PlanStepEvent,
  StepEvent,
  SubtaskEvent,
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
export interface KnowledgeEvent extends KnowledgeReport {}
export interface FileChangedHandlerEvent extends FileChangedEvent {}
export interface ToolOutputHandlerEvent extends ToolOutputEvent {}
export interface StepHandlerEvent extends StepEvent {}
export interface SubtaskHandlerEvent extends SubtaskEvent {}
export interface PlanStepHandlerEvent extends PlanStepEvent {}

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
  onKnowledge?: (e: KnowledgeEvent) => void;
  onFileChanged?: (e: FileChangedHandlerEvent) => void;
  onToolOutput?: (e: ToolOutputHandlerEvent) => void;
  onStep?: (e: StepHandlerEvent) => void;
  onSubtask?: (e: SubtaskHandlerEvent) => void;
  onPlanStep?: (e: PlanStepHandlerEvent) => void;
  onSkillsChanged?: (e: { name: string; path: string }) => void;
}

export function parseEvent<T>(raw: unknown): T {
  if (typeof raw === "string") return JSON.parse(raw) as T;
  return raw as T;
}
