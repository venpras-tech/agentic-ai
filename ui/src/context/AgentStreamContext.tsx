import { createContext, useContext } from "react";
import { useAgentStream } from "../hooks/useAgentStream";

export type AgentStreamValue = ReturnType<typeof useAgentStream>;

const AgentStreamContext = createContext<AgentStreamValue | null>(null);

export function useAgentStreamContext() {
  const ctx = useContext(AgentStreamContext);
  if (!ctx) throw new Error("useAgentStreamContext must be used within AgentStreamProvider");
  return ctx;
}

export function AgentStreamProvider({ children }: { children: React.ReactNode }) {
  const value = useAgentStream();
  return <AgentStreamContext.Provider value={value}>{children}</AgentStreamContext.Provider>;
}