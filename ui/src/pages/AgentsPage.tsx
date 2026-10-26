import { useSearchParams } from "react-router-dom";
import ConnectedAgentsRail from "../components/agents/ConnectedAgentsRail";
import ModelsWorkspace from "../components/agents/ModelsWorkspace";
import "./AgentsPage.css";

function AgentsPage() {
  const [params] = useSearchParams();
  const focusLocal = params.get("focus") === "local";
  const preset = params.get("preset");

  return (
    <div className="agents-page">
      <header className="agents-hero">
        <div className="agents-hero-text">
          <h1 className="agents-title">Agent backends</h1>
          <p className="agents-subtitle">
            Route the coding agent through external APIs (OpenAI, Claude, Gemini, OpenRouter) or local
            servers (Ollama, vLLM, LiteLLM). The same tools, MCP, and workspace — different brain.
          </p>
        </div>
      </header>
      <ConnectedAgentsRail />
      <ModelsWorkspace
        initialProvider={focusLocal ? "local" : preset ?? undefined}
        embedded
      />
    </div>
  );
}

export default AgentsPage;
