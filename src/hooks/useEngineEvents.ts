import { useEffect, useRef } from "react";

import { api } from "../lib/ipc";
import type { EngineHandlers } from "../lib/events";

/**
 * Subscribes once to the host's engine events and routes them to the latest
 * handler closures via a ref (avoids re-subscribing on every render).
 */
export function useEngineEvents(handlers: EngineHandlers) {
  const latest = useRef(handlers);
  latest.current = handlers;

  useEffect(() => {
    let unlisten: (() => void) | null = null;
    let disposed = false;

    const ref = latest;
    api.subscribeEngineEvents({
      onToken: (e) => ref.current.onToken(e),
      onStarted: (e) => ref.current.onStarted(e),
      onDone: (e) => ref.current.onDone(e),
      onError: (e) => ref.current.onError(e),
      onModelLoaded: (e) => ref.current.onModelLoaded(e),
      onLoadProgress: (e) => ref.current.onLoadProgress(e),
      onTool: (e) => ref.current.onTool?.(e),
      onAborted: (e) => ref.current.onAborted?.(e),
      onPermission: (e) => ref.current.onPermission?.(e),
      onQuestion: (e) => ref.current.onQuestion?.(e),
      onKnowledge: (e) => ref.current.onKnowledge?.(e),
      onFileChanged: (e) => ref.current.onFileChanged?.(e),
      onToolOutput: (e) => ref.current.onToolOutput?.(e),
      onStep: (e) => ref.current.onStep?.(e),
      onSubtask: (e) => ref.current.onSubtask?.(e),
      onPlanStep: (e) => ref.current.onPlanStep?.(e),
      onSkillsChanged: (e) => ref.current.onSkillsChanged?.(e),
      onTodoUpdate: (e) => ref.current.onTodoUpdate?.(e),
      onBgTask: (e) => ref.current.onBgTask?.(e),
      onWorkspaceChanged: (e) => ref.current.onWorkspaceChanged?.(e),
    }).then((un) => {
      if (disposed) un();
      else unlisten = un;
    });

    return () => {
      disposed = true;
      unlisten?.();
    };
  }, []);
}
