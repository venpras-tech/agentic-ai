export type ProviderCategory = "local" | "local-server" | "cloud" | "gateway";

export type ProviderPreset = {
  id: string;
  name: string;
  url: string;
  key: string;
  category: ProviderCategory;
  /** Recommended for full agent tool loops (vs chat-only). */
  agentCapable?: boolean;
  /** Supports image attachments in chat when configured. */
  vision?: boolean;
  /** Shown in Models UI when the endpoint must be customized per account. */
  urlHint?: string;
  /** Suggested default model id when listing is unavailable. */
  defaultModel?: string;
};

export const PROVIDER_CATEGORY_LABELS: Record<ProviderCategory, string> = {
  local: "On-device",
  "local-server": "Local API servers",
  cloud: "Cloud APIs",
  gateway: "Gateways & routers",
};

export const PROVIDER_CATALOG: ProviderPreset[] = [
  {
    id: "local",
    name: "Local (GGUF)",
    url: "",
    key: "",
    category: "local",
    agentCapable: false,
  },
  {
    id: "ollama",
    name: "Ollama",
    url: "http://localhost:11434/v1",
    key: "Optional — Ollama needs no API key by default",
    category: "local-server",
    agentCapable: true,
    defaultModel: "llama3.2",
  },
  {
    id: "lmstudio",
    name: "LM Studio",
    url: "http://localhost:1234/v1",
    key: "Optional — LM Studio runs fully offline",
    category: "local-server",
    agentCapable: true,
  },
  {
    id: "vllm",
    name: "vLLM",
    url: "http://localhost:8000/v1",
    key: "Optional for open local deployments",
    category: "local-server",
    agentCapable: true,
    urlHint: "OpenAI-compatible server (vLLM, TGI, etc.)",
  },
  {
    id: "localai",
    name: "LocalAI",
    url: "http://localhost:8080/v1",
    key: "Optional",
    category: "local-server",
    agentCapable: true,
  },
  {
    id: "litellm",
    name: "LiteLLM proxy",
    url: "http://localhost:4000/v1",
    key: "Optional — set in LiteLLM config",
    category: "local-server",
    agentCapable: true,
    urlHint: "Unified proxy to many cloud providers",
  },
  {
    id: "openai",
    name: "OpenAI",
    url: "https://api.openai.com/v1",
    key: "sk-… from https://platform.openai.com/api-keys",
    category: "cloud",
    agentCapable: true,
    vision: true,
    defaultModel: "gpt-4o-mini",
  },
  {
    id: "anthropic",
    name: "Anthropic (Claude)",
    url: "https://api.anthropic.com/v1",
    key: "sk-ant-… from https://console.anthropic.com",
    category: "cloud",
    agentCapable: true,
    vision: true,
    defaultModel: "claude-sonnet-4-20250514",
  },
  {
    id: "google",
    name: "Google Gemini",
    url: "https://generativelanguage.googleapis.com/v1beta",
    key: "AIza… from https://aistudio.google.com",
    category: "cloud",
    agentCapable: true,
    vision: true,
    defaultModel: "gemini-2.0-flash",
  },
  {
    id: "azure",
    name: "Azure OpenAI",
    url: "https://YOUR-RESOURCE.openai.azure.com/openai/deployments/YOUR-DEPLOYMENT",
    key: "Azure API key from the portal",
    category: "cloud",
    agentCapable: true,
    vision: true,
    urlHint: "Use your resource + deployment path (OpenAI-compatible)",
  },
  {
    id: "github",
    name: "GitHub Models",
    url: "https://models.inference.ai.azure.com",
    key: "GitHub PAT with models scope",
    category: "cloud",
    agentCapable: true,
    defaultModel: "gpt-4o",
  },
  {
    id: "openrouter",
    name: "OpenRouter",
    url: "https://openrouter.ai/api/v1",
    key: "sk-or-… from https://openrouter.ai/keys",
    category: "gateway",
    agentCapable: true,
    vision: true,
  },
  {
    id: "deepseek",
    name: "DeepSeek",
    url: "https://api.deepseek.com/v1",
    key: "sk-… from https://platform.deepseek.com",
    category: "cloud",
    agentCapable: true,
    defaultModel: "deepseek-chat",
  },
  {
    id: "xai",
    name: "xAI (Grok)",
    url: "https://api.x.ai/v1",
    key: "xai-… from https://console.x.ai",
    category: "cloud",
    agentCapable: true,
  },
  {
    id: "groq",
    name: "Groq",
    url: "https://api.groq.com/openai/v1",
    key: "gsk_… from https://console.groq.com/keys",
    category: "cloud",
    agentCapable: true,
  },
  {
    id: "mistral",
    name: "Mistral",
    url: "https://api.mistral.ai/v1",
    key: "… from https://console.mistral.ai/api-keys",
    category: "cloud",
    agentCapable: true,
  },
  {
    id: "together",
    name: "Together AI",
    url: "https://api.together.xyz/v1",
    key: "… from https://api.together.ai/settings/api-keys",
    category: "cloud",
    agentCapable: true,
  },
  {
    id: "fireworks",
    name: "Fireworks AI",
    url: "https://api.fireworks.ai/inference/v1",
    key: "fw_… from https://fireworks.ai/account/api-keys",
    category: "cloud",
    agentCapable: true,
  },
  {
    id: "cerebras",
    name: "Cerebras",
    url: "https://api.cerebras.ai/v1",
    key: "csk-… from https://cloud.cerebras.ai",
    category: "cloud",
    agentCapable: true,
  },
  {
    id: "sambanova",
    name: "SambaNova",
    url: "https://api.sambanova.ai/v1",
    key: "… from https://cloud.sambanova.ai/apis",
    category: "cloud",
    agentCapable: true,
  },
  {
    id: "perplexity",
    name: "Perplexity",
    url: "https://api.perplexity.ai",
    key: "pplx-… from https://www.perplexity.ai/settings/api",
    category: "cloud",
    agentCapable: false,
  },
  {
    id: "nvidia",
    name: "NVIDIA NIM",
    url: "https://integrate.api.nvidia.com/v1",
    key: "nvapi-… from https://build.nvidia.com",
    category: "cloud",
    agentCapable: true,
  },
  {
    id: "huggingface",
    name: "Hugging Face",
    url: "https://router.huggingface.co/v1",
    key: "hf_… from https://huggingface.co/settings/tokens",
    category: "gateway",
    agentCapable: true,
  },
  {
    id: "cloudflare",
    name: "Cloudflare Workers AI",
    url: "https://api.cloudflare.com/client/v4/accounts/YOUR_ACCOUNT_ID/ai/v1",
    key: "Cloudflare API token",
    category: "cloud",
    agentCapable: true,
    urlHint: "Replace YOUR_ACCOUNT_ID in the URL",
  },
  {
    id: "moonshot",
    name: "Moonshot (Kimi)",
    url: "https://api.moonshot.cn/v1",
    key: "sk-… from https://platform.moonshot.cn/console/api-keys",
    category: "cloud",
    agentCapable: true,
  },
  {
    id: "zai",
    name: "Z.ai (GLM)",
    url: "https://api.z.ai/api/paas/v4",
    key: "… from https://z.ai/manage-apikey/apikey-list",
    category: "cloud",
    agentCapable: true,
  },
  {
    id: "hyperbolic",
    name: "Hyperbolic",
    url: "https://api.hyperbolic.xyz/v1",
    key: "… from https://app.hyperbolic.xyz/settings",
    category: "cloud",
    agentCapable: true,
  },
  {
    id: "custom",
    name: "Custom (OpenAI-compatible)",
    url: "",
    key: "Required if your endpoint enforces auth",
    category: "gateway",
    agentCapable: true,
    urlHint: "vLLM, Bedrock OpenAI shim, corporate gateway, etc.",
  },
];

export const providerById: Record<string, ProviderPreset | undefined> = Object.fromEntries(
  PROVIDER_CATALOG.map((p) => [p.id, p]),
);

const WIRE_KIND_IDS = new Set([
  "openai",
  "anthropic",
  "openrouter",
  "google",
  "ollama",
  "lmstudio",
  "deepseek",
  "xai",
  "groq",
  "mistral",
  "together",
  "fireworks",
  "cerebras",
  "sambanova",
  "perplexity",
  "nvidia",
  "huggingface",
  "moonshot",
  "zai",
  "hyperbolic",
]);

export const kindByProvider: Record<string, string> = Object.fromEntries(
  PROVIDER_CATALOG.map((p) => [p.id, WIRE_KIND_IDS.has(p.id) ? p.id : "custom"]),
);

export function providerUrl(id: string): string {
  return providerById[id]?.url ?? "";
}

export function providerKeyHint(id: string): string {
  return providerById[id]?.key || "Required for cloud providers";
}

export function catalogKind(id: string): string {
  return kindByProvider[id] ?? "custom";
}

export function providersByCategory(): { category: ProviderCategory; label: string; items: ProviderPreset[] }[] {
  const order: ProviderCategory[] = ["local", "local-server", "cloud", "gateway"];
  return order.map((category) => ({
    category,
    label: PROVIDER_CATEGORY_LABELS[category],
    items: PROVIDER_CATALOG.filter((p) => p.category === category),
  }));
}
