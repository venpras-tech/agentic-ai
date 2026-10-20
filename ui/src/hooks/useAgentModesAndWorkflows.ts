import { useEffect, useState } from "react";
import { api, type AgentModeInfo, type WorkflowInfo } from "../lib/ipc";

export interface AgentModesAndWorkflows {
  modes: AgentModeInfo[];
  workflows: WorkflowInfo[];
  customMode: string;
  setCustomMode: (name: string) => void;
  activeWorkflow: string;
  setActiveWorkflow: (name: string) => void;
}

/**
 * Loads the user-defined modes (`.ai/modes/*.md`) and workflows
 * (`.ai/workflows/*.md`) so the composer can offer them and pass the selection
 * through to the backend, which gates the allowed tools.
 */
export function useAgentModesAndWorkflows(): AgentModesAndWorkflows {
  const [modes, setModes] = useState<AgentModeInfo[]>([]);
  const [workflows, setWorkflows] = useState<WorkflowInfo[]>([]);
  const [customMode, setCustomMode] = useState("");
  const [activeWorkflow, setActiveWorkflow] = useState("");

  useEffect(() => {
    let cancelled = false;
    void api
      .modesLoad()
      .then((list) => {
        if (!cancelled) setModes(list ?? []);
      })
      .catch(() => {});
    void api
      .workflowsLoad()
      .then((list) => {
        if (!cancelled) setWorkflows(list ?? []);
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, []);

  return { modes, workflows, customMode, setCustomMode, activeWorkflow, setActiveWorkflow };
}
