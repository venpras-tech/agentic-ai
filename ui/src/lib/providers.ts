export type ProviderPreset = {
  id: string;
  name: string;
  url: string;
  key: string;
};

export const PROVIDERS: ProviderPreset[] = [
  { id: "local", name: "Local (GGUF)", url: "", key: "" },
  {
    id: "ollama",
    name: "Ollama",
    url: "http://localhost:11434/v1",
    key: "Optional – Ollama needs no API key by default",
  },
  {
    id: "lmstudio",
    name: "LM Studio",
    url: "http://localhost:1234/v1",
    key: "Optional – LM Studio runs fully offline",
  },
  {
    id: "openai",
    name: "OpenAI",
    url: "https://api.openai.com/v1",
    key: "sk-… from https://platform.openai.com/api-keys",
  },
  {
    id: "anthropic",
    name: "Anthropic",
    url: "https://api.anthropic.com/v1",
    key: "sk-ant-… from https://console.anthropic.com",
  },
  {
    id: "google",
    name: "Google",
    url: "https://generativelanguage.googleapis.com/v1beta",
    key: "AIza… from https://aistudio.google.com",
  },
  {
    id: "openrouter",
    name: "OpenRouter",
    url: "https://openrouter.ai/api/v1",
    key: "sk-or-… from https://openrouter.ai/keys",
  },
  {
    id: "deepseek",
    name: "DeepSeek",
    url: "https://api.deepseek.com/v1",
    key: "sk-… from https://platform.deepseek.com",
  },
  {
    id: "xai",
    name: "xAI (Grok)",
    url: "https://api.x.ai/v1",
    key: "xai-… from https://console.x.ai",
  },
  {
    id: "groq",
    name: "Groq",
    url: "https://api.groq.com/openai/v1",
    key: "gsk_… from https://console.groq.com/keys",
  },
  {
    id: "mistral",
    name: "Mistral",
    url: "https://api.mistral.ai/v1",
    key: "… from https://console.mistral.ai/api-keys",
  },
  {
    id: "together",
    name: "Together AI",
    url: "https://api.together.xyz/v1",
    key: "… from https://api.together.ai/settings/api-keys",
  },
  {
    id: "fireworks",
    name: "Fireworks AI",
    url: "https://api.fireworks.ai/inference/v1",
    key: "fw_… from https://fireworks.ai/account/api-keys",
  },
  {
    id: "cerebras",
    name: "Cerebras",
    url: "https://api.cerebras.ai/v1",
    key: "csk-… from https://cloud.cerebras.ai",
  },
  {
    id: "sambanova",
    name: "SambaNova",
    url: "https://api.sambanova.ai/v1",
    key: "… from https://cloud.sambanova.ai/apis",
  },
  {
    id: "perplexity",
    name: "Perplexity",
    url: "https://api.perplexity.ai",
    key: "pplx-… from https://www.perplexity.ai/settings/api",
  },
  {
    id: "nvidia",
    name: "NVIDIA NIM",
    url: "https://integrate.api.nvidia.com/v1",
    key: "nvapi-… from https://build.nvidia.com",
  },
  {
    id: "huggingface",
    name: "Hugging Face",
    url: "https://router.huggingface.co/v1",
    key: "hf_… from https://huggingface.co/settings/tokens",
  },
  {
    id: "moonshot",
    name: "Moonshot (Kimi)",
    url: "https://api.moonshot.cn/v1",
    key: "sk-… from https://platform.moonshot.cn/console/api-keys",
  },
  {
    id: "zai",
    name: "Z.ai (GLM)",
    url: "https://api.z.ai/api/paas/v4",
    key: "… from https://z.ai/manage-apikey/apikey-list",
  },
  {
    id: "hyperbolic",
    name: "Hyperbolic",
    url: "https://api.hyperbolic.xyz/v1",
    key: "… from https://app.hyperbolic.xyz/settings",
  },
  {
    id: "custom",
    name: "Custom",
    url: "",
    key: "Required if your endpoint enforces auth",
  },
];

export const providerById: Record<string, ProviderPreset | undefined> = Object.fromEntries(
  PROVIDERS.map((p) => [p.id, p]),
);

export const kindByProvider: Record<string, string> = Object.fromEntries(
  PROVIDERS.map((p) => [p.id, p.id]),
);

export function providerUrl(id: string): string {
  return providerById[id]?.url ?? "";
}

export function providerKeyHint(id: string): string {
  return providerById[id]?.key || "Required for cloud providers";
}
