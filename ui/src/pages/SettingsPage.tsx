import { useState, useEffect, useRef, useCallback } from "react";
import { useShell, useModel, useAgentSettings } from "../context/AppContext";
import { api, isTauriRuntime, type McpServerConfig, type McpTransport, type McpProbeResult, type Skill } from "../lib/ipc";
import { suggestLlmParams, type SystemSnapshot } from "../lib/modelSuggest";
import {
  mergeLocalConfig,
  normalizedConfig,
  sanitizeLocalConfig,
  validateLocalModelConfig,
  type LocalModelConfig,
} from "../lib/localModelConfig";
import ProviderRegistry from "../components/ProviderRegistry";
import { getVersion } from "@tauri-apps/api/app";
import "./SettingsPage.css";

const APP_VERSION_FALLBACK = "0.1.0";

type LlmSettings = import("../context/AppContext").LlmSettings;

type McpForm = {
  name: string;
  transport: McpTransport;
  bin: string;
  args: string;
  url: string;
  headers: string;
  env: string;
  allowedTools: string;
  enabled: boolean;
};

const emptyForm: McpForm = {
  name: "",
  transport: "stdio",
  bin: "",
  args: "",
  url: "",
  headers: "",
  env: "",
  allowedTools: "",
  enabled: true,
};

const SAMPLE_MCP_JSON = [
  {
    name: "filesystem",
    bin: "npx",
    args: ["-y", "@modelcontextprotocol/server-filesystem", "C:\\path\\to\\your\\project"],
    env: {},
    allowedTools: [],
    enabled: true,
  },
];

const SAMPLE_SKILL_MD = `---
name: hashing
description: Compute SHA-256 checksums of strings or files to verify integrity, deduplicate content, or answer "what is the hash of …?" questions.
---

# Hashing

Use this when the user asks for a hash, checksum, digest, or wants to verify file integrity.

## Compute a string hash

1. Take the exact string the user provided (do not trim or alter it).
2. Compute its SHA-256 digest and show it as lowercase hex.
3. Label the result: \`SHA-256(string) = <hex>\`.

## Compute a file hash

1. Locate the file (respect the workspace root; ask before touching files outside it).
2. Run the system's checksum tool (\`sha256sum <file>\` on Linux/macOS, \`Get-FileHash <file> -Algorithm SHA256\` on Windows), or hash the file bytes directly.
3. Report the hex digest with the file path.

## Rules

- Always state which algorithm you used.
- Never invent a hash — compute it or say you cannot.
- If the user asks for a different algorithm (MD5, SHA-1, SHA-512), switch to it and say so.
`;

function InfoIcon() {
  return (
    <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
      <circle cx="12" cy="12" r="10" />
      <path d="M12 16v-4M12 8h.01" />
    </svg>
  );
}

function parseArgs(text: string): string[] {
  const tokens = text.match(/"([^"]*)"|'([^']*)'|\S+/g) ?? [];
  return tokens.map((t) => t.replace(/^(["'])|(["'])$/g, "").replace(/^(["'])/, "").replace(/(["'])$/, "")).filter(Boolean);
}

function parseEnv(text: string): Record<string, string> {
  const out: Record<string, string> = {};
  for (const line of text.split(/\r?\n/)) {
    const idx = line.indexOf("=");
    if (idx > 0 && line.slice(0, idx).trim()) {
      out[line.slice(0, idx).trim()] = line.slice(idx + 1).trim();
    }
  }
  return out;
}

function parseHeaders(text: string): Record<string, string> {
  return parseEnv(text);
}

function formatStringMap(map: Record<string, string> | undefined): string {
  return Object.entries(map || {})
    .map(([k, v]) => `${k}=${v}`)
    .join("\n");
}

function validateCatalogJson(json: unknown):
  | { ok: true; servers: McpServerConfig[] }
  | { ok: false; error: string } {
  let raw = json;
  if (raw && typeof raw === "object" && !Array.isArray(raw)) {
    raw = [raw];
  }
  if (!Array.isArray(raw)) {
    return { ok: false, error: "Expected a server object or an array of servers." };
  }
  const servers: McpServerConfig[] = [];
  const seen = new Set<string>();
  for (const item of raw as unknown[]) {
    if (!item || typeof item !== "object") {
      return { ok: false, error: "Each entry must be a JSON object." };
    }
    const s = item as Record<string, unknown>;
    const name = typeof s.name === "string" ? s.name.trim() : "";
    if (!name) {
      return { ok: false, error: "Every server needs a non-empty `name` string." };
    }
    if (seen.has(name)) {
      return { ok: false, error: `Duplicate server name \`${name}\`.` };
    }
    seen.add(name);
    const transport: McpTransport = s.transport === "http" ? "http" : "stdio";
    const bin = typeof s.bin === "string" ? s.bin.trim() : "";
    const url = typeof s.url === "string" ? s.url.trim() : "";
    if (transport === "stdio" && !bin) {
      return { ok: false, error: `Server \`${name}\` needs a non-empty \`bin\` command.` };
    }
    if (transport === "http" && !url) {
      return { ok: false, error: `Server \`${name}\` needs a non-empty \`url\` for the http transport.` };
    }
    const args = Array.isArray(s.args)
      ? s.args.filter((a): a is string => typeof a === "string")
      : [];
    const env: Record<string, string> = {};
    if (s.env && typeof s.env === "object" && !Array.isArray(s.env)) {
      for (const [k, v] of Object.entries(s.env as Record<string, unknown>)) {
        env[k] = String(v);
      }
    }
    const headers: Record<string, string> = {};
    if (s.headers && typeof s.headers === "object" && !Array.isArray(s.headers)) {
      for (const [k, v] of Object.entries(s.headers as Record<string, unknown>)) {
        headers[k] = String(v);
      }
    }
    const allowedTools = Array.isArray(s.allowedTools)
      ? s.allowedTools.filter((a): a is string => typeof a === "string")
      : [];
    servers.push({
      name,
      transport,
      bin,
      args,
      url,
      headers,
      env,
      allowedTools,
      enabled: s.enabled !== false,
    });
  }
  return { ok: true, servers };
}

function defaultCatalogJson(servers: McpServerConfig[]): string {
  return JSON.stringify(servers, null, 2) || "[]";
}

function BanIcon() {
  return (
    <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
      <circle cx="12" cy="12" r="10" />
      <line x1="4.93" y1="4.93" x2="19.07" y2="19.07" />
    </svg>
  );
}

function SettingsPage() {
  const { theme, setTheme, projectPath } = useShell();
  const { llmSettings, loadLlmSettings, saveLlmSettings, modelMeta, activeModelConfig, activeLocalModelPath, saveLocalModelConfig } = useModel();
  const { verify, setVerify, yolo, setYolo, maxSteps, setMaxSteps, tokenBudget, setTokenBudget, decompose, setDecompose, preferChatComplete, setPreferChatComplete } = useAgentSettings();
  const [activeSection, setActiveSection] = useState("general");
  const [formOpen, setFormOpen] = useState(false);
  const [servers, setServers] = useState<McpServerConfig[]>([]);
  const [loaded, setLoaded] = useState(false);
  const [editIndex, setEditIndex] = useState<number | null>(null);
  const [form, setForm] = useState<McpForm>(emptyForm);
  const [dirty, setDirty] = useState(false);
  const [saving, setSaving] = useState(false);
  const [mcpMode, setMcpMode] = useState<"form" | "json">("form");
  const [jsonText, setJsonText] = useState("");
  const [mcpMsg, setMcpMsg] = useState<{ text: string; error?: boolean } | null>(null);
  const [probes, setProbes] = useState<Record<string, { busy?: boolean; result?: McpProbeResult; error?: string }>>({});
  const [skills, setSkills] = useState<Skill[]>([]);
  const [rulesSources, setRulesSources] = useState<string[]>([]);
  const [skillsLoaded, setSkillsLoaded] = useState(false);
  const [skillMsg, setSkillMsg] = useState<{ text: string; error?: boolean } | null>(null);
  const [skillGlobal, setSkillGlobal] = useState(false);
  const [skillBusy, setSkillBusy] = useState(false);
  const [draftTheme, setDraftTheme] = useState(theme);
  const [modelsDir, setModelsDir] = useState("");
  const [draftModelsDir, setDraftModelsDir] = useState("");
  const [defaultModelsDir, setDefaultModelsDir] = useState("");
  const [draftLlm, setDraftLlm] = useState<Partial<LlmSettings>>({});
  const [draftLocal, setDraftLocal] = useState<Partial<LocalModelConfig>>({});
  const [localErrors, setLocalErrors] = useState<string[]>([]);
  const [toast, setToast] = useState<{ text: string; error?: boolean } | null>(null);
  const toastTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const [systemInfo, setSystemInfo] = useState<SystemSnapshot | null>(null);
  const [mcpInfoOpen, setMcpInfoOpen] = useState(false);
  const [skillInfoOpen, setSkillInfoOpen] = useState(false);
  const [appVersion, setAppVersion] = useState(APP_VERSION_FALLBACK);
  const [apiSrv, setApiSrv] = useState({ enabled: true, port: 8080, routeAssistant: true });
  const [apiSrvStatus, setApiSrvStatus] = useState<{ running: boolean; port: number | null }>({
    running: false,
    port: null,
  });
  const [apiSrvBusy, setApiSrvBusy] = useState(false);
  const [agentApiInfoOpen, setAgentApiInfoOpen] = useState(false);
  const [apiSrvTesting, setApiSrvTesting] = useState(false);

  useEffect(() => {
    if (!isTauriRuntime()) return;
    getVersion()
      .then(setAppVersion)
      .catch(() => {});
  }, []);

  useEffect(() => {
    if (!isTauriRuntime()) return;
    api
      .systemInfo()
      .then((s) =>
        setSystemInfo({
          cores: s.cores,
          totalRamGb: s.totalRamGb,
          availableRamGb: s.availableRamGb,
          gpu: s.gpu,
          gpuEnabled: s.gpuEnabled,
        }),
      )
      .catch(() => {});
  }, []);

  const notify = (text: string, error?: boolean) => {
    setToast({ text, error });
    if (toastTimerRef.current) clearTimeout(toastTimerRef.current);
    toastTimerRef.current = setTimeout(() => setToast(null), 2800);
  };

  useEffect(() => () => {
    if (toastTimerRef.current) clearTimeout(toastTimerRef.current);
  }, []);

  useEffect(() => {
    setDraftTheme(theme);
  }, [theme]);

  useEffect(() => {
    if (!isTauriRuntime()) return;
    api
      .modelsDirInfo()
      .then((info) => {
        setModelsDir(info.custom ? info.modelsDir : "");
        setDraftModelsDir(info.custom ? info.modelsDir : "");
        setDefaultModelsDir(info.defaultDir);
      })
      .catch(() => {});
  }, []);

  useEffect(() => {
    if (!isTauriRuntime()) return;
    api
      .settingsLoad()
      .then((s) => {
        const la = (s.localApiServer ?? {}) as Partial<{ enabled: boolean; port: number; routeAssistant: boolean }>;
        setApiSrv({
          enabled: la.enabled ?? true,
          port: typeof la.port === "number" && la.port > 0 && la.port < 65536 ? la.port : 8080,
          routeAssistant: la.routeAssistant ?? true,
        });
      })
      .catch(() => {});
  }, []);

  useEffect(() => {
    if (!isTauriRuntime()) return;
    let cancelled = false;
    const refresh = () =>
      api
        .apiServerStatus()
        .then((st) => {
          if (!cancelled) setApiSrvStatus(st);
        })
        .catch(() => {});
    refresh();
    return () => {
      cancelled = true;
    };
  }, [modelMeta]);

  const saveApiServer = useCallback(
    async (next: { enabled: boolean; port: number; routeAssistant: boolean }) => {
      if (!isTauriRuntime()) return;
      try {
        const current = (await api.settingsLoad()) || {};
        await api.settingsSave({
          ...current,
          localApiServer: {
            enabled: next.enabled,
            port: next.port,
            routeAssistant: next.routeAssistant,
          },
        });
      } catch {
        notify("Could not save the local model server settings.", true);
      }
    },
    [],
  );

  const apiSrvStart = async () => {
    if (apiSrvBusy) return;
    setApiSrvBusy(true);
    setApiSrvStatus((s) => ({ ...s, running: false }));
    try {
      const st = await api.apiServerStart(apiSrv.port);
      setApiSrvStatus(st);
      notify(st.running ? "Local model server started." : "Local model server could not be started.", !st.running);
    } catch (err) {
      notify(`Could not start the server: ${err instanceof Error ? err.message : String(err)}`, true);
      setApiSrvStatus((s) => ({ ...s, running: false }));
    } finally {
      setApiSrvBusy(false);
    }
  };

  const apiSrvStop = async () => {
    if (apiSrvBusy) return;
    setApiSrvBusy(true);
    try {
      const st = await api.apiServerStop();
      setApiSrvStatus(st);
      notify(!st.running ? "Local model server stopped." : "");
    } catch (err) {
      notify(`Could not stop the server: ${err instanceof Error ? err.message : String(err)}`, true);
    } finally {
      setApiSrvBusy(false);
    }
  };

  const apiSrvSetPort = (value: string) => {
    const parsed = Number(value);
    setApiSrv((s) => ({ ...s, port: Number.isFinite(parsed) && parsed >= 1 && parsed <= 65535 ? parsed : s.port }));
    if (Number.isFinite(parsed) && parsed >= 1 && parsed <= 65535) {
      void saveApiServer({ ...apiSrv, port: parsed });
    }
  };

  const apiSrvTest = async () => {
    if (apiSrvTesting) return;
    setApiSrvTesting(true);
    const port = apiSrvStatus.port ?? apiSrv.port;
    try {
      const res = await fetch(`http://127.0.0.1:${port}/v1/models`);
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      const data = (await res.json()) as { data?: unknown[] };
      const count = Array.isArray(data.data) ? data.data.length : 0;
      notify(count > 0 ? `Server OK — ${count} model(s) available.` : "Server OK — no model loaded.", count === 0);
    } catch (err) {
      notify(`Could not reach the server: ${err instanceof Error ? err.message : String(err)}`, true);
    } finally {
      setApiSrvTesting(false);
    }
  };

  const apiSrvPort = apiSrvStatus.port ?? apiSrv.port;

  useEffect(() => {
    setDraftLlm(llmSettings);
  }, [llmSettings]);

  const seedLocalDraft = useCallback(() => {
    const base: Partial<LocalModelConfig> = {};
    if (llmSettings.contextSize != null) base.contextSize = llmSettings.contextSize;
    if (llmSettings.nThreads != null) base.nThreads = llmSettings.nThreads;
    if (llmSettings.nGpuLayers != null) base.nGpuLayers = llmSettings.nGpuLayers;
    const merged = mergeLocalConfig(base, activeModelConfig);
    // Only model-load fields are user-adjustable now — the model runs on its
    // native sampler defaults ("use llm as is"). Drop any sampler fields that
    // older per-model configs persisted so they can't re-surface later.
    const draft: Partial<LocalModelConfig> = {
      contextSize: merged.contextSize,
      nThreads: merged.nThreads,
      nGpuLayers: merged.nGpuLayers,
    };
    setDraftLocal(draft as Partial<LocalModelConfig>);
    setLocalErrors([]);
  }, [llmSettings, activeModelConfig]);

  // Seed the Local Model section once the persisted configs have surfaced
  // (mirrors how draftLlm mirrors llmSettings), then leave edits alone.
  const localSeededRef = useRef(false);
  useEffect(() => {
    if (localSeededRef.current) return;
    const t = setTimeout(() => {
      localSeededRef.current = true;
      seedLocalDraft();
    }, 250);
    return () => clearTimeout(t);
  }, [seedLocalDraft]);

  const setLlmNum = (key: keyof LlmSettings) => (e: React.ChangeEvent<HTMLInputElement>) => {
    const num = e.target.value ? Number(e.target.value) : undefined;
    setDraftLlm((prev) => ({ ...prev, [key]: num }));
  };

  const setLocalNum = (key: keyof LocalModelConfig) => (e: React.ChangeEvent<HTMLInputElement>) => {
    const num = e.target.value ? Number(e.target.value) : undefined;
    setDraftLocal((prev) => ({ ...prev, [key]: num }));
  };

  const browseModelsDir = async () => {
    if (!isTauriRuntime()) return;
    try {
      const picked = await api.pickWorkspaceFolder();
      if (picked) {
        setDraftModelsDir(picked);
        notify("Folder selected — click Apply to save.");
      }
    } catch {
      notify("Couldn't open the folder picker.", true);
    }
  };

  const applyGeneral = async () => {
    setTheme(draftTheme);
    const next = draftModelsDir.trim();
    try {
      const current = (await api.settingsLoad()) || {};
      await api.settingsSave({ ...current, modelsDir: next });
      setModelsDir(next);
      notify(next ? "Theme and models folder saved." : "Theme saved; models folder reset to default.");
    } catch (err) {
      notify(String((err as { message?: string })?.message ?? err), true);
    }
  };

  const resetGeneral = () => {
    setDraftTheme(theme);
    setDraftModelsDir(modelsDir);
    notify("Changes reset to the current settings.");
  };

  const applyLlm = () => {
    // Drop stale `null`/NaN values from configs persisted before null
    // sanitizing so they validate as "unset" instead of 0.
    const clean = sanitizeLocalConfig(draftLocal as Record<string, unknown>);
    if (Object.keys(clean).length !== Object.keys(draftLocal).length) {
      setDraftLocal(clean);
    }
    const errors = validateLocalModelConfig(clean);
    if (errors.length) {
      setLocalErrors(errors);
      notify(errors[0], true);
      return;
    }
    setLocalErrors([]);
    // Keep the legacy load fields in sync so the pre-config auto-load path
    // and older consumers keep working unchanged.
    const loadPatch: Partial<LlmSettings> = {};
    for (const [from, to] of LEGACY_LOAD_SYNC) {
      if (clean[from] !== undefined) {
        (loadPatch[to] as number | undefined) = clean[from] as number;
      }
    }
    saveLlmSettings({ ...draftLlm, ...loadPatch });
    saveLocalModelConfig(activeLocalModelPath, normalizedConfig(clean));
    notify("LLM settings saved — applied on the next model load.");
  };

  const suggestions = modelMeta ? suggestLlmParams(modelMeta, systemInfo) : null;

  const applySuggestions = () => {
    if (!suggestions) return;
    const merged = {
      ...draftLlm,
      contextSize: suggestions.params.contextSize,
      nGpuLayers: suggestions.params.nGpuLayers,
      nThreads: suggestions.params.nThreads,
    };
    setDraftLlm(merged);
    saveLlmSettings(merged);
    // Mirror the load fields into the active model's config so they win on
    // reload (the per-model config overrides the legacy settings). Sampling
    // params are intentionally left alone — the model runs on engine defaults.
    const cfgPatch: Partial<LocalModelConfig> = {
      contextSize: suggestions.params.contextSize,
      nGpuLayers: suggestions.params.nGpuLayers,
      nThreads: suggestions.params.nThreads,
    };
    setDraftLocal((prev) => mergeLocalConfig(prev as Partial<LocalModelConfig>, cfgPatch));
    saveLocalModelConfig(activeLocalModelPath, mergeLocalConfig(activeModelConfig, cfgPatch));
    notify("Suggested parameters applied to the loaded model.");
  };

  const resetLlm = () => {
    setDraftLlm(llmSettings);
    seedLocalDraft();
    setLocalErrors([]);
    void loadLlmSettings();
    notify("LLM settings reset to the last saved values.");
  };

  const applyMcp = async () => {
    await handleSaveAll();
  };

  const resetMcp = async () => {
    setMcpMsg(null);
    try {
      const list = await api.mcpLoadCatalog();
      setServers(list ?? []);
      setDirty(false);
      setJsonText(defaultCatalogJson(list ?? []));
      notify("MCP catalog reset to the last saved state.");
    } catch (err) {
      const msg = String((err as { message?: string })?.message ?? err);
      setMcpMsg({ text: msg, error: true });
      notify(msg, true);
    }
  };

  const resetSkills = async () => {
    setSkillMsg(null);
    try {
      applyReport(await api.knowledgeScan());
      notify("Skill state reloaded from disk.");
    } catch (err) {
      const msg = String((err as { message?: string })?.message ?? err);
      setSkillMsg({ text: msg, error: true });
      notify(msg, true);
    }
  };

  const copySample = async (text: string, label: string) => {
    try {
      await navigator.clipboard.writeText(text);
      notify(`Copied ${label} sample to the clipboard.`);
    } catch {
      notify("Couldn't access the clipboard.", true);
    }
  };

  const sampleMcpArgs = () => {
    const root = (projectPath || "C:\\path\\to\\your\\project").replaceAll("\\", "\\\\");
    return ["-y", "@modelcontextprotocol/server-filesystem", root];
  };

  const addSampleMcpServer = () => {
    const entry: McpServerConfig = {
      name: "filesystem",
      transport: "stdio",
      bin: "npx",
      args: sampleMcpArgs(),
      url: "",
      headers: {},
      env: {},
      allowedTools: [],
      enabled: true,
    };
    if (servers.some((s) => s.name === entry.name)) {
      setMcpMsg({ text: "A \"filesystem\" server is already in the catalog.", error: true });
      notify("Sample MCP server already added.", true);
      return;
    }
    setServers([...servers, entry]);
    setDirty(true);
    setMcpMsg({ text: "Sample filesystem server added — save changes to persist, then test it in chat." });
    notify("Sample MCP server added.");
  };

  const installSampleSkill = async () => {
    setSkillBusy(true);
    setSkillMsg(null);
    try {
      applyReport(await api.skillInstallSample("hashing", SAMPLE_SKILL_MD, skillGlobal));
      setSkillMsg({ text: "Sample skill installed — it is now active in the agent context. Ask it to hash something to test." });
      notify("Sample skill installed.");
    } catch (err) {
      const msg = String((err as { message?: string })?.message ?? err);
      setSkillMsg({ text: msg, error: true });
      notify(msg, true);
    } finally {
      setSkillBusy(false);
    }
  };

  useEffect(() => {
    if (!isTauriRuntime()) {
      setLoaded(true);
      return;
    }
    api
      .mcpLoadCatalog()
      .then((list) => {
        setServers(list ?? []);
        setLoaded(true);
      })
      .catch((err) => {
        setMcpMsg({ text: String((err as { message?: string })?.message ?? err), error: true });
        setLoaded(true);
      });
  }, []);

  const loadKnowledge = useCallback(() => {
    if (!isTauriRuntime()) {
      setSkillsLoaded(true);
      return;
    }
    api
      .knowledgeReport()
      .then((report) => {
        setSkills(report.skills);
        setRulesSources(report.rulesSources);
        setSkillsLoaded(true);
      })
      .catch((err) => {
        setSkillMsg({ text: String((err as { message?: string })?.message ?? err), error: true });
        setSkillsLoaded(true);
      });
  }, []);

  useEffect(() => {
    loadKnowledge();
  }, [loadKnowledge]);

  useEffect(() => {
    const onChanged = () => loadKnowledge();
    window.addEventListener("vencoder-skills-changed", onChanged);
    return () => window.removeEventListener("vencoder-skills-changed", onChanged);
  }, [loadKnowledge]);

  const openAdd = () => {
    setForm(emptyForm);
    setEditIndex(null);
    setFormOpen(true);
  };

  const openEdit = (idx: number) => {
    const s = servers[idx];
    setForm({
      name: s.name,
      transport: s.transport === "http" ? "http" : "stdio",
      bin: s.bin,
      args: s.args.join(" "),
      url: s.url || "",
      headers: formatStringMap(s.headers),
      env: formatStringMap(s.env),
      allowedTools: (s.allowedTools || []).join(", "),
      enabled: s.enabled,
    });
    setEditIndex(idx);
    setFormOpen(true);
  };

  const removeServer = (idx: number) => {
    if (!window.confirm(`Remove MCP server "${servers[idx].name}" from the catalog?`)) return;
    setServers((prev) => prev.filter((_, i) => i !== idx));
    setDirty(true);
  };

  const toggleEnabled = (idx: number) => {
    setServers((prev) => prev.map((s, i) => (i === idx ? { ...s, enabled: !s.enabled } : s)));
    setDirty(true);
  };

  const probeServer = async (name: string) => {
    setProbes((prev) => ({ ...prev, [name]: { busy: true } }));
    try {
      const result = await api.mcpServerProbe(name);
      setProbes((prev) => ({ ...prev, [name]: { result } }));
    } catch (err) {
      const msg = String((err as { message?: string })?.message ?? err);
      setProbes((prev) => ({ ...prev, [name]: { error: msg } }));
    }
  };

  const toggleServerTool = (idx: number, tool: string) => {
    setServers((prev) =>
      prev.map((s, i) => {
        if (i !== idx) return s;
        const known = probes[s.name]?.result?.tools ?? [];
        const current = s.allowedTools.length === 0 ? [...known] : [...s.allowedTools];
        const set = new Set(current);
        if (set.has(tool)) set.delete(tool);
        else set.add(tool);
        const next = known.length > 0 && known.every((t) => set.has(t)) ? [] : Array.from(set);
        return { ...s, allowedTools: next };
      }),
    );
    setDirty(true);
  };

  const handleSaveAll = async () => {
    setSaving(true);
    setMcpMsg(null);
    try {
      await api.mcpSaveCatalog(servers);
      setDirty(false);
      setMcpMsg({ text: "MCP catalog saved." });
      notify("MCP catalog saved.");
    } catch (err) {
      const msg = String((err as { message?: string })?.message ?? err);
      setMcpMsg({ text: msg, error: true });
      notify(msg, true);
    } finally {
      setSaving(false);
    }
  };

  const switchToJson = () => {
    setJsonText(defaultCatalogJson(servers));
    setMcpMsg(null);
    setMcpMode("json");
  };

  const switchToForm = () => {
    setMcpMode("form");
    setMcpMsg(null);
  };

  const formatJson = () => {
    try {
      setJsonText(JSON.stringify(JSON.parse(jsonText), null, 2));
    } catch (err) {
      setMcpMsg({ text: `Invalid JSON: ${(err as { message?: string }).message ?? err}`, error: true });
    }
  };

  const applyJson = () => {
    let parsed: unknown;
    try {
      parsed = JSON.parse(jsonText);
    } catch (err) {
      setMcpMsg({ text: `Invalid JSON: ${(err as { message?: string }).message ?? err}`, error: true });
      return;
    }
    const result = validateCatalogJson(parsed);
    if (!result.ok) {
      setMcpMsg({ text: result.error, error: true });
      return;
    }
    setServers(result.servers);
    setDirty(true);
    setMcpMsg({ text: `Parsed ${result.servers.length} server${result.servers.length === 1 ? "" : "s"} — save changes to persist.` });
  };

  const applyReport = (report: { skills: Skill[]; rulesSources: string[] }) => {
    setSkills(report.skills);
    setRulesSources(report.rulesSources);
  };

  const importSkill = async (kind: "file" | "folder") => {
    if (!isTauriRuntime()) return;
    setSkillBusy(true);
    setSkillMsg(null);
    try {
      const report = await api.skillImportDialog(kind, skillGlobal);
      if (report) {
        applyReport(report);
        setSkillMsg({ text: "Skill installed — now active in the agent context." });
      }
    } catch (err) {
      setSkillMsg({ text: String((err as { message?: string })?.message ?? err), error: true });
    } finally {
      setSkillBusy(false);
    }
  };

  const toggleSkill = async (name: string, active: boolean) => {
    setSkillBusy(true);
    setSkillMsg(null);
    try {
      applyReport(await api.skillSetActive(name, active));
    } catch (err) {
      setSkillMsg({ text: String((err as { message?: string })?.message ?? err), error: true });
    } finally {
      setSkillBusy(false);
    }
  };

  const uninstallSkill = async (name: string) => {
    if (!window.confirm(`Delete skill "${name}" from disk?`)) return;
    setSkillBusy(true);
    setSkillMsg(null);
    try {
      applyReport(await api.skillUninstall(name));
      setSkillMsg({ text: `Skill "${name}" deleted.` });
    } catch (err) {
      setSkillMsg({ text: String((err as { message?: string })?.message ?? err), error: true });
    } finally {
      setSkillBusy(false);
    }
  };

  const rescanSkills = async () => {
    setSkillBusy(true);
    setSkillMsg(null);
    try {
      applyReport(await api.knowledgeScan());
    } catch (err) {
      setSkillMsg({ text: String((err as { message?: string })?.message ?? err), error: true });
    } finally {
      setSkillBusy(false);
    }
  };

  const submitForm = () => {
    const name = form.name.trim();
    if (!name) {
      setMcpMsg({ text: "Server name is required.", error: true });
      return;
    }
    if (form.transport === "http") {
      if (!form.url.trim()) {
        setMcpMsg({ text: "URL is required for the http transport.", error: true });
        return;
      }
    } else if (!form.bin.trim()) {
      setMcpMsg({ text: "Command (executable) is required for the stdio transport.", error: true });
      return;
    }
    const entry: McpServerConfig = {
      name,
      transport: form.transport,
      bin: form.transport === "stdio" ? form.bin.trim() : "",
      args: form.transport === "stdio" ? parseArgs(form.args) : [],
      url: form.transport === "http" ? form.url.trim() : "",
      headers: form.transport === "http" ? parseHeaders(form.headers) : {},
      env: form.transport === "stdio" ? parseEnv(form.env) : {},
      allowedTools: form.allowedTools.split(",").map((t) => t.trim()).filter(Boolean),
      enabled: form.enabled,
    };
    setServers((prev) => {
      const next = [...prev];
      if (editIndex != null) next[editIndex] = entry;
      else next.push(entry);
      return next;
    });
    setFormOpen(false);
    setMcpMsg(null);
    setDirty(true);
  };

  return (
    <div className="settings-view">
      <h2 className="settings-title">Settings</h2>
      {toast && (
        <div className={`settings-toast${toast.error ? " settings-toast-error" : ""}`} role="status">
          {toast.error && <span className="settings-toast-icon">!</span>}
          {toast.text}
        </div>
      )}
      <div className="settings-layout">
        <nav className="settings-nav" role="tablist">
          {[
            { id: "general", label: "General" },
            { id: "providers", label: "Providers" },
            { id: "skills", label: "Skills" },
            { id: "llm", label: "LLM" },
            { id: "agent", label: "Agent" },
            { id: "mcp", label: "MCP" },
            { id: "about", label: "About" },
          ].map((tab) => (
            <button
              key={tab.id}
              type="button"
              className={`settings-nav-item ${activeSection === tab.id ? "active" : ""}`}
              onClick={() => setActiveSection(tab.id)}
              role="tab"
            >
              {tab.label}
            </button>
          ))}
        </nav>
        <div className="settings-panels">
          {activeSection === "general" && (
            <div className="settings-panel">
              <h3 className="settings-panel-heading">General</h3>
              <div className="settings-field">
                <label className="settings-label">Theme</label>
                <div className="settings-theme-options">
                  {[
                    { id: "light", label: "Light" },
                    { id: "dark", label: "Dark" },
                    { id: "midnight", label: "Midnight" },
                    { id: "ocean", label: "Ocean" },
                    { id: "forest", label: "Forest" },
                    { id: "sepia", label: "Sepia" },
                    { id: "system", label: "System" },
                    { id: "high-contrast", label: "High contrast" },
                  ].map((opt) => (
                    <label key={opt.id} className="settings-radio">
                      <input
                        type="radio"
                        name="theme"
                        value={opt.id}
                        checked={draftTheme === opt.id}
                        onChange={() => setDraftTheme(opt.id)}
                      />
                      <span className={`settings-theme-swatch swatch-${opt.id}`} aria-hidden="true" />
                      <span>{opt.label}</span>
                    </label>
                  ))}
                </div>
                <p className="settings-hint">
                  System follows your OS appearance. High contrast increases contrast for accessibility.
                </p>
              </div>
              <div className="settings-field">
                <label className="settings-label" htmlFor="setting-models-dir">Models folder</label>
                <div className="settings-input-row">
                  <input
                    id="setting-models-dir"
                    className="settings-input"
                    placeholder={defaultModelsDir || "Default (app data folder)"}
                    value={draftModelsDir}
                    onChange={(e) => setDraftModelsDir(e.target.value)}
                    spellCheck={false}
                  />
                  <button type="button" className="settings-btn" onClick={() => void browseModelsDir()}>
                    Browse…
                  </button>
                  {draftModelsDir && (
                    <button type="button" className="settings-btn" onClick={() => setDraftModelsDir("")}>
                      Default
                    </button>
                  )}
                </div>
                <p className="settings-hint">
                  Where downloaded models are saved and scanned for the model list. Leave blank to use the
                  default folder{defaultModelsDir ? ` (${defaultModelsDir})` : ""}.
                </p>
              </div>
              <div className="settings-actions">
                <button type="button" className="settings-btn settings-btn-primary" onClick={() => void applyGeneral()}>
                  Apply
                </button>
                <button type="button" className="settings-btn" onClick={resetGeneral}>
                  Reset
                </button>
              </div>
            </div>
          )}

          {activeSection === "providers" && (
            <div className="settings-panel">
              <ProviderRegistry />
            </div>
          )}

          {activeSection === "skills" && (
            <div className="settings-panel">
              <div className="settings-heading-row">
                <h3 className="settings-panel-heading">Skills & rules</h3>
                <button
                  type="button"
                  className={`settings-info-btn${skillInfoOpen ? " active" : ""}`}
                  title="Sample skills & how to test them"
                  aria-expanded={skillInfoOpen}
                  onClick={() => setSkillInfoOpen((o) => !o)}
                >
                  <InfoIcon />
                </button>
              </div>
              <p className="settings-hint">
                Skills are Markdown capability packs (<code>.ai/skills/*.md</code> or <code>{'{config}/skills/'}</code>) the agent
                loads into its context. They are active by default — toggle them off here, install new
                ones from a file or folder, or remove them.
              </p>

              {skillInfoOpen && (
                <div className="settings-sample-panel">
                  <div className="settings-sample-head">
                    <span className="settings-sample-title">Sample skill for testing</span>
                    <div className="settings-sample-actions">
                      <button type="button" className="settings-btn settings-btn-small" onClick={() => void copySample(SAMPLE_SKILL_MD, "skill")}>
                        Copy
                      </button>
                      <button type="button" className="settings-btn settings-btn-small settings-btn-primary" disabled={skillBusy} onClick={() => void installSampleSkill()}>
                        Install sample skill
                      </button>
                    </div>
                  </div>
                  <p className="settings-hint">
                    This creates <code>.ai/skills/hashing.md</code> in your project (or the global skills dir if
                    "Install globally" is ticked) and immediately activates it — then say <em>"what is the
                    sha-256 of hello"</em> in chat to test. Written SKILL.md files live in
                    <code> .ai/skills/</code>; they must start with a <code>---</code> block containing{" "}
                    <code>name</code> and <code>description</code>.
                  </p>
                  <pre className="settings-sample-code" spellCheck={false}>{SAMPLE_SKILL_MD}</pre>
                </div>
              )}

              <div className="settings-skill-toolbar">
                <button type="button" className="settings-btn settings-btn-primary" disabled={skillBusy} onClick={() => void importSkill("file")}>
                  <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2"><path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4"/><polyline points="17 8 12 3 7 8"/><line x1="12" y1="3" x2="12" y2="15"/></svg>
                  Import .md file
                </button>
                <button type="button" className="settings-btn" disabled={skillBusy} onClick={() => void importSkill("folder")} title="A folder containing SKILL.md">
                  Import folder
                </button>
                <button type="button" className="settings-btn" disabled={skillBusy} onClick={() => void rescanSkills()} title="Re-scan workspace and global skills dirs">
                  <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2"><path d="M21 2v6h-6"/><path d="M3 12a9 9 0 0 1 15-6.7L21 8"/><path d="M3 22v-6h6"/><path d="M21 12a9 9 0 0 1-15 6.7L3 16"/></svg>
                  Rescan
                </button>
                <label className="settings-check settings-skill-scope">
                  <input type="checkbox" checked={skillGlobal} onChange={(e) => setSkillGlobal(e.target.checked)} />
                  <span>Install globally (all projects)</span>
                </label>
              </div>

              {skillMsg && (
                <p className={`settings-mcp-status-line ${skillMsg.error ? "error" : ""}`}>{skillMsg.text}</p>
              )}

              {!skillsLoaded ? (
                <p className="settings-hint">Loading skills…</p>
              ) : skills.length === 0 ? (
                <div className="settings-skill-empty">
                  <p>No skills found yet.</p>
                  <p className="settings-hint">
                    Import a <code>.md</code> skill file or a folder containing <code>SKILL.md</code>. Skills
                    also load from <code>.ai/skills/</code> in your project.
                  </p>
                </div>
              ) : (
                <div className="settings-skill-list">
                  {skills.map((s) => (
                    <div key={s.name} className={`settings-skill-item${s.active ? "" : " disabled"}`}>
                      <div className="settings-skill-main">
                        <div className="settings-skill-title">
                          <span className={`settings-mcp-badge${s.active ? " on" : " off"}`}>{s.active ? "active" : "off"}</span>
                          <span className="settings-skill-name">{s.name}</span>
                          {s.tags.length > 0 && (
                            <span className="settings-skill-tags">{s.tags.map((t) => `#${t}`).join(" ")}</span>
                          )}
                        </div>
                        {s.description && <div className="settings-skill-desc">{s.description}</div>}
                        <div className="settings-skill-meta">
                          <span title={s.source}>{s.source}</span>
                          {s.globs.length > 0 && <span>matches: {s.globs.join(", ")}</span>}
                          <span>{s.content.length.toLocaleString()} chars</span>
                        </div>
                      </div>
                      <div className="settings-skill-actions">
                        <label className="settings-check" title={s.active ? "Deactivate" : "Activate"}>
                          <input type="checkbox" disabled={skillBusy} checked={s.active} onChange={() => void toggleSkill(s.name, !s.active)} />
                          <span>Active</span>
                        </label>
                        <button type="button" className="settings-btn settings-btn-small settings-btn-danger" disabled={skillBusy} onClick={() => void uninstallSkill(s.name)} title="Delete skill">
                          <BanIcon />
                        </button>
                      </div>
                    </div>
                  ))}
                </div>
              )}

              {(rulesSources.length > 0 || skillMsg) && (
                <div className="settings-skill-rules">
                  <h4 className="settings-skill-rules-heading">Loaded rules ({rulesSources.length})</h4>
                  <div className="settings-skill-rules-list">
                    {rulesSources.map((src) => (
                      <code key={src} className="settings-skill-rule-src">{src}</code>
                    ))}
                  </div>
                </div>
              )}

              <div className="settings-actions">
                <button type="button" className="settings-btn settings-btn-primary" disabled={skillBusy} onClick={() => { void rescanSkills(); notify("Skills rescanned and applied."); }}>
                  Apply
                </button>
                <button type="button" className="settings-btn settings-btn-danger" disabled={skillBusy} onClick={() => void resetSkills()}>
                  Reset
                </button>
              </div>
            </div>
          )}

          {activeSection === "llm" && (
            <div className="settings-panel">
              <h3 className="settings-panel-heading">LLM parameters</h3>
              <p className="settings-hint">
                Configure generation defaults and model-loading settings. Parameters take effect on the next model load.
              </p>

              {modelMeta ? (
                <div className="settings-llm-suggest">
                  <div className="settings-llm-suggest-head">
                    <div className="settings-llm-suggest-model">
                      <strong className="settings-llm-suggest-name">{modelMeta.name}</strong>
                      <span className="settings-llm-suggest-arch">{modelMeta.architecture}</span>
                    </div>
                    <button type="button" className="settings-btn settings-btn-primary" onClick={applySuggestions}>
                      Apply suggested values
                    </button>
                  </div>
                  <div className="settings-llm-suggest-meta">
                    <span>Architecture: {modelMeta.architecture}</span>
                    <span>Params: {(modelMeta.nParams / 1e9).toFixed(1)}B</span>
                    <span>Train ctx: {modelMeta.nCtxTrain.toLocaleString()}</span>
                    <span>Layers: {modelMeta.nLayer}</span>
                    <span>KV cache: ~{suggestions ? suggestions.kvGiB.toFixed(1) : "?"} GiB @ suggested ctx</span>
                  </div>
                  {suggestions && (
                    <div className="settings-llm-suggest-values">
                      <div className="settings-llm-suggest-col">
                        <span className="settings-llm-suggest-k">Context size</span>
                        <span className="settings-llm-suggest-v">{suggestions.params.contextSize?.toLocaleString() ?? "—"}</span>
                      </div>
                      <div className="settings-llm-suggest-col">
                        <span className="settings-llm-suggest-k">GPU layers</span>
                        <span className="settings-llm-suggest-v">{suggestions.params.nGpuLayers ?? "—"}</span>
                      </div>
                      <div className="settings-llm-suggest-col">
                        <span className="settings-llm-suggest-k">Threads</span>
                        <span className="settings-llm-suggest-v">{suggestions.params.nThreads ?? "—"}</span>
                      </div>
                    </div>
                  )}
                  {suggestions?.fitsRam === false && (
                    <p className="settings-llm-suggest-warn">
                      Warning: the model plus its suggested context may exceed available RAM — consider a smaller
                      context or a smaller quant.
                    </p>
                  )}
                  {suggestions?.bitsPerWeight != null && suggestions.bitsPerWeight < 4 && (
                    <p className={`settings-llm-suggest-warn${suggestions.quantLevel === "heavy" ? " settings-llm-suggest-warn-strong" : ""}`}>
                      Heavy quantisation detected (~{suggestions.bitsPerWeight.toFixed(1)} bits/weight) — this model is
                      more prone to hallucination. A Q4_K_M or larger quant will be noticeably more accurate.
                    </p>
                  )}
                  {suggestions?.notes.map((n) => (
                    <p key={n} className="settings-llm-suggest-note">{n}</p>
                  ))}
                </div>
              ) : (
                <p className="settings-hint">
                  Load a model to get parameter suggestions tuned for it (context cap, GPU layers, threads).
                </p>
              )}

              <h4 style={{ margin: "18px 0 10px", fontSize: "0.85rem", fontWeight: 600, color: "var(--text-primary)" }}>
                Model loading
              </h4>
              <div className="settings-llm-grid">
                <LNum
                  id="setting-ctx-length"
                  label="Context length"
                  value={draftLocal.contextSize}
                  onChange={(v) => setDraftLocal((p) => ({ ...p, contextSize: v }))}
                  min={128}
                  max={modelMeta ? Math.max(128, modelMeta.nCtxTrain) : 2097152}
                  step={256}
                  placeholder="auto"
                  hint={
                    modelMeta
                      ? `Default: auto — up to ${modelMeta.nCtxTrain.toLocaleString()} for this model`
                      : "Default: auto (model's built-in)"
                  }
                />
                <LNum
                  id="setting-gpu-offload"
                  label="GPU offload"
                  value={draftLocal.nGpuLayers}
                  onChange={(v) => setDraftLocal((p) => ({ ...p, nGpuLayers: v }))}
                  min={0}
                  max={modelMeta ? Math.max(1, modelMeta.nLayer) : 16384}
                  step={1}
                  placeholder="0"
                  hint={
                    modelMeta
                      ? `Default: 0 (CPU only) — max ${modelMeta.nLayer} layers for this model`
                      : "Default: 0 (CPU only)"
                  }
                />
                <LNum
                  id="setting-cpu-threads"
                  label="CPU threads"
                  value={draftLocal.nThreads}
                  onChange={(v) => setDraftLocal((p) => ({ ...p, nThreads: v }))}
                  min={1}
                  max={1024}
                  step={1}
                  placeholder="auto"
                  hint="Default: auto (all available cores)"
                />
              </div>
              <p className="settings-hint">
                Apply on the next load of this model. Unset value = auto (context uses the model&rsquo;s
                built-in, offload runs on CPU, threads use all cores).
              </p>
              {localErrors.length > 0 && (
                <div className="settings-local-errors">
                  {localErrors.map((e) => (
                    <p key={e} className="settings-local-error">{e}</p>
                  ))}
                </div>
              )}

              <div className="settings-actions">
                <button type="button" className="settings-btn settings-btn-primary" onClick={applyLlm}>
                  Apply
                </button>
                <button type="button" className="settings-btn settings-btn-danger" onClick={resetLlm}>
                  Reset
                </button>
              </div>

              <h4 style={{ margin: "18px 0 10px", fontSize: "0.85rem", fontWeight: 600, color: "var(--text-primary)" }}>
                Local model server
              </h4>
              <div className="settings-local-server">
                <div className="settings-local-server-head">
                  <strong>OpenAI-compatible endpoint</strong>
                  <span className="settings-local-server-head-right">
                    <span className={`settings-local-server-status${apiSrvStatus.running ? " on" : ""}`}>
                      {apiSrvStatus.running
                        ? `Running — http://127.0.0.1:${apiSrvStatus.port ?? apiSrv.port}/v1`
                        : "Stopped"}
                    </span>
                    <button
                      type="button"
                      className={`settings-info-btn${agentApiInfoOpen ? " active" : ""}`}
                      title="Agent automation API & curl examples"
                      aria-expanded={agentApiInfoOpen}
                      onClick={() => setAgentApiInfoOpen((o) => !o)}
                    >
                      <InfoIcon />
                    </button>
                  </span>
                </div>
                <p className="settings-hint">
                  Serves the loaded GGUF over HTTP at{" "}
                  <code>http://127.0.0.1:{apiSrv.port}/v1</code> — the same way
                  Ollama and LM Studio do, so any OpenAI client (or this app&rsquo;s own
                  assistant) can connect to it as an OpenAI-compatible endpoint.
                </p>
                <div className="settings-local-server-controls">
                  <label className="settings-llm-label" htmlFor="setting-api-port">
                    Port
                  </label>
                  <input
                    id="setting-api-port"
                    type="number"
                    min={1}
                    max={65535}
                    className="settings-input"
                    value={apiSrv.port}
                    onChange={(e) => apiSrvSetPort(e.target.value)}
                  />
                </div>
                <div className="settings-llm-toggle-row">
                  <label className="settings-llm-label" htmlFor="setting-api-auto">
                    Start automatically when a local model loads
                  </label>
                  <input
                    id="setting-api-auto"
                    type="checkbox"
                    className="settings-llm-toggle"
                    checked={apiSrv.enabled}
                    onChange={(e) => {
                      const enabled = e.target.checked;
                      setApiSrv((s) => ({ ...s, enabled }));
                      void saveApiServer({ ...apiSrv, enabled });
                    }}
                  />
                </div>
                <div className="settings-llm-toggle-row">
                  <label className="settings-llm-label" htmlFor="setting-api-route">
                    Route the in-app assistant through this server (like Ollama / LM Studio)
                  </label>
                  <input
                    id="setting-api-route"
                    type="checkbox"
                    className="settings-llm-toggle"
                    checked={apiSrv.routeAssistant}
                    onChange={(e) => {
                      const routeAssistant = e.target.checked;
                      setApiSrv((s) => ({ ...s, routeAssistant }));
                      void saveApiServer({ ...apiSrv, routeAssistant });
                    }}
                  />
                </div>
                <div className="settings-server-actions">
                  <button
                    type="button"
                    className="settings-btn"
                    onClick={() => void apiSrvTest()}
                    disabled={apiSrvTesting}
                  >
                    {apiSrvTesting ? "Testing…" : "Test connection"}
                  </button>
                  {apiSrvStatus.running ? (
                    <button type="button" className="settings-btn settings-btn-danger" onClick={() => void apiSrvStop()} disabled={apiSrvBusy}>
                      {apiSrvBusy ? "Stopping…" : "Stop server"}
                    </button>
                  ) : (
                    <button type="button" className="settings-btn settings-btn-primary" onClick={() => void apiSrvStart()} disabled={apiSrvBusy}>
                      {apiSrvBusy ? "Starting…" : "Start server"}
                    </button>
                  )}
                </div>
                {agentApiInfoOpen && (
                  <div className="settings-sample-panel">
                    <div className="settings-sample-head">
                      <span className="settings-sample-title">Agent automation API (curl)</span>
                    </div>
                    <p className="settings-hint">
                      In addition to chat completions, the server exposes agentic task execution. A task
                      starts by number and streams progress on the <code>agent://*</code> event bus; poll the
                      trace endpoint for a step-by-step log.
                    </p>
                    <pre className="settings-sample-code">{`# Start an agentic task (returns { "sessionId": <n> })
curl -s http://127.0.0.1:${apiSrvPort}/v1/agent/tasks \\
  -H "Content-Type: application/json" \\
  -d '{"prompt":"add a hello() function to util.ts","maxTokens":2048}'

# Fetch the recorded per-step run trace
curl -s http://127.0.0.1:${apiSrvPort}/v1/agent/sessions/1/trace

# Plain chat completion (OpenAI-compatible)
curl -s http://127.0.0.1:${apiSrvPort}/v1/chat/completions \\
  -H "Content-Type: application/json" \\
  -d '{"messages":[{"role":"user","content":"hi"}]}'`}</pre>
                  </div>
                )}
              </div>
            </div>
          )}

          {activeSection === "mcp" && (
            <div className="settings-panel">
              <div className="settings-heading-row">
                <h3 className="settings-panel-heading">MCP (Model Context Protocol)</h3>
                <button
                  type="button"
                  className={`settings-info-btn${mcpInfoOpen ? " active" : ""}`}
                  title="Sample MCP server & how to test it"
                  aria-expanded={mcpInfoOpen}
                  onClick={() => setMcpInfoOpen((o) => !o)}
                >
                  <InfoIcon />
                </button>
              </div>
              <p className="settings-hint">
                MCP servers let the agent use external tools (filesystem, browser, databases, …) by name.
                Each entry spawns a command over stdio. Changes below are written to the catalog and take effect on the next tool call.
              </p>

              {mcpInfoOpen && (
                <div className="settings-sample-panel">
                  <div className="settings-sample-head">
                    <span className="settings-sample-title">Sample MCP server for testing</span>
                    <div className="settings-sample-actions">
                      <button type="button" className="settings-btn settings-btn-small" onClick={() => void copySample(JSON.stringify(SAMPLE_MCP_JSON, null, 2), "MCP")}>
                        Copy JSON
                      </button>
                      <button type="button" className="settings-btn settings-btn-small settings-btn-primary" onClick={addSampleMcpServer}>
                        Add sample filesystem server
                      </button>
                    </div>
                  </div>
                  <p className="settings-hint">
                    Uses the official <code>@modelcontextprotocol/server-filesystem</code> (runs via
                    <code> npx</code>, Node.js required). It lets the agent read and write files in your
                    project folder. Add it, click <strong>Save changes</strong>, then ask in chat{" "}
                    <em>"list the files in my project using the filesystem tool"</em>. The path uses your
                    currently open project when available.
                  </p>
                  <pre className="settings-sample-code" spellCheck={false}>{JSON.stringify(SAMPLE_MCP_JSON, null, 2)}</pre>
                </div>
              )}

              <div className="settings-mcp-modes">
                <button
                  type="button"
                  className={`settings-md-tab${mcpMode === "form" ? " active" : ""}`}
                  onClick={switchToForm}
                >
                  Form
                </button>
                <button
                  type="button"
                  className={`settings-md-tab${mcpMode === "json" ? " active" : ""}`}
                  onClick={switchToJson}
                >
                  JSON
                </button>
              </div>

              <div className="settings-mcp-toolbar">
                {mcpMode === "form" && (
                  <button type="button" className="settings-btn settings-btn-primary" onClick={openAdd}>
                    <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.2"><line x1="12" y1="5" x2="12" y2="19"/><line x1="5" y1="12" x2="19" y2="12"/></svg>
                    Add MCP server
                  </button>
                )}
                {dirty && (
                  <button type="button" className="settings-btn" onClick={() => void handleSaveAll()} disabled={saving}>
                    {saving ? "Saving…" : "Save changes"}
                  </button>
                )}
              </div>

              {mcpMsg && (
                <p className={`settings-mcp-status-line ${mcpMsg.error ? "error" : ""}`}>{mcpMsg.text}</p>
              )}

              {mcpMode === "json" ? (
                <div className="settings-mcp-json-panel">
                  <div className="settings-mcp-json-field">
                    <label className="settings-label" htmlFor="mcp-json">MCP server catalog (JSON)</label>
                    <textarea
                      id="mcp-json"
                      className="settings-mcp-json-input"
                      spellCheck={false}
                      value={jsonText}
                      onChange={(e) => setJsonText(e.target.value)}
                      placeholder={'[\n  {\n    "name": "playwright",\n    "transport": "stdio",\n    "bin": "npx",\n    "args": ["@playwright/mcp@latest"],\n    "env": {},\n    "allowedTools": [],\n    "enabled": true\n  },\n  {\n    "name": "remote",\n    "transport": "http",\n    "url": "https://host.example.com/mcp",\n    "headers": { "Authorization": "Bearer ..." },\n    "enabled": true\n  }\n]'}
                    />
                    <p className="settings-hint">
                      Accepts a single server object or an array. Fields: <code>name</code> (required),
                      <code> transport</code> (<code>stdio</code>/<code>http</code>), <code>bin</code>+<code>args</code>+<code>env</code> (stdio),
                      <code> url</code>+<code>headers</code> (http), <code>allowedTools</code>, <code>enabled</code>.
                    </p>
                  </div>
                  <div className="settings-mcp-json-actions">
                    <button type="button" className="settings-btn" onClick={formatJson}>Format</button>
                    <button type="button" className="settings-btn settings-btn-primary" onClick={applyJson}>Validate &amp; apply</button>
                  </div>
                </div>
              ) : (
                <>
                  {formOpen && (
                    <div className="settings-mcp-form">
                  <div className="settings-mcp-form-grid">
                    <div className="settings-field">
                      <label className="settings-label" htmlFor="mcp-name">Name</label>
                      <input
                        id="mcp-name"
                        className="settings-input"
                        placeholder="e.g. playwright"
                        value={form.name}
                        onChange={(e) => setForm({ ...form, name: e.target.value })}
                      />
                      <p className="settings-hint">Unique name the agent uses to call this server.</p>
                    </div>
                    <div className="settings-field">
                      <label className="settings-label" htmlFor="mcp-transport">Transport</label>
                      <select
                        id="mcp-transport"
                        className="settings-input"
                        value={form.transport}
                        onChange={(e) => setForm({ ...form, transport: e.target.value as McpTransport })}
                      >
                        <option value="stdio">stdio (local command)</option>
                        <option value="http">http (remote URL)</option>
                      </select>
                      <p className="settings-hint">How the agent connects to this server.</p>
                    </div>
                    {form.transport === "stdio" ? (
                      <>
                        <div className="settings-field">
                          <label className="settings-label" htmlFor="mcp-bin">Command</label>
                          <input
                            id="mcp-bin"
                            className="settings-input"
                            placeholder="e.g. npx"
                            value={form.bin}
                            onChange={(e) => setForm({ ...form, bin: e.target.value })}
                          />
                          <p className="settings-hint">Executable on PATH or absolute path.</p>
                        </div>
                        <div className="settings-field settings-mcp-full">
                          <label className="settings-label" htmlFor="mcp-args">Arguments</label>
                          <input
                            id="mcp-args"
                            className="settings-input"
                            placeholder='e.g. "@playwright/mcp@latest" --browser chromium'
                            value={form.args}
                            onChange={(e) => setForm({ ...form, args: e.target.value })}
                          />
                        </div>
                        <div className="settings-field">
                          <label className="settings-label" htmlFor="mcp-env">Environment variables</label>
                          <textarea
                            id="mcp-env"
                            className="settings-input settings-mcp-env"
                            placeholder="API_KEY=sk-...\nDB_URL=postgres://localhost:5432/app"
                            value={form.env}
                            onChange={(e) => setForm({ ...form, env: e.target.value })}
                          />
                          <p className="settings-hint">One KEY=VALUE per line.</p>
                        </div>
                      </>
                    ) : (
                      <>
                        <div className="settings-field settings-mcp-full">
                          <label className="settings-label" htmlFor="mcp-url">URL</label>
                          <input
                            id="mcp-url"
                            className="settings-input"
                            placeholder="e.g. https://host.example.com/mcp"
                            value={form.url}
                            onChange={(e) => setForm({ ...form, url: e.target.value })}
                          />
                          <p className="settings-hint">Streamable HTTP endpoint of the MCP server.</p>
                        </div>
                        <div className="settings-field settings-mcp-full">
                          <label className="settings-label" htmlFor="mcp-headers">Headers</label>
                          <textarea
                            id="mcp-headers"
                            className="settings-input settings-mcp-env"
                            placeholder="Authorization=Bearer sk-..."
                            value={form.headers}
                            onChange={(e) => setForm({ ...form, headers: e.target.value })}
                          />
                          <p className="settings-hint">One KEY=VALUE per line (e.g. auth tokens).</p>
                        </div>
                      </>
                    )}
                    <div className="settings-field">
                      <label className="settings-label" htmlFor="mcp-tools">Allowed tools</label>
                      <input
                        id="mcp-tools"
                        className="settings-input"
                        placeholder="e.g. browser_navigate, click*, … (blank = allow all)"
                        value={form.allowedTools}
                        onChange={(e) => setForm({ ...form, allowedTools: e.target.value })}
                      />
                      <p className="settings-hint">Comma separated; trailing * matches a prefix. Leave blank to allow everything.</p>
                    </div>
                  </div>
                  <div className="settings-mcp-check-row">
                    <label className="settings-check">
                      <input type="checkbox" checked={form.enabled} onChange={(e) => setForm({ ...form, enabled: e.target.checked })} />
                      <span>Enabled</span>
                    </label>
                  </div>
                  <div className="settings-mcp-form-actions">
                    <button type="button" className="settings-btn settings-btn-primary" onClick={submitForm}>
                      {editIndex != null ? "Update server" : "Add server"}
                    </button>
                    <button type="button" className="settings-btn" onClick={() => setFormOpen(false)}>Cancel</button>
                  </div>
                </div>
              )}

              {!loaded ? (
                <p className="settings-hint">Loading catalog…</p>
              ) : servers.length === 0 && !formOpen ? (
                <div className="settings-mcp-empty">
                  <p>No MCP servers configured yet.</p>
                  <p className="settings-hint">
                    Popular servers: <code>@playwright/mcp@latest</code> (browser automation), <code>@modelcontextprotocol/server-filesystem</code> (file access).
                  </p>
                </div>
              ) : (
                <div className="settings-mcp-list">
                  {servers.map((s, idx) => (
                    <div key={s.name} className={`settings-mcp-item${s.enabled ? "" : " disabled"}`}>
                      <div className="settings-mcp-item-main">
                        <div className="settings-mcp-item-title">
                          <span className={`settings-mcp-badge${s.enabled ? " on" : " off"}`}>{s.enabled ? "enabled" : "disabled"}</span>
                          <span className="settings-mcp-item-name">{s.name}</span>
                          <span className="settings-mcp-item-transport">{s.transport === "http" ? "http" : "stdio"}</span>
                        </div>
                        {s.transport === "http" ? (
                          <div className="settings-mcp-item-cmd" title={s.url || ""}>
                            <code>{s.url}</code>
                          </div>
                        ) : (
                          <div className="settings-mcp-item-cmd" title={`${s.bin} ${s.args.join(" ")}`}>
                            <code>{s.bin}</code> {s.args.join(" ")}
                          </div>
                        )}
                        {s.allowedTools.length > 0 && (
                          <div className="settings-mcp-item-tools">Tools: {s.allowedTools.join(", ")}</div>
                        )}
                        {Object.keys(s.env || {}).length > 0 && (
                          <div className="settings-mcp-item-tools">Env: {Object.keys(s.env).join(", ")}</div>
                        )}
                      </div>
                      <div className="settings-mcp-item-actions">
                        <label className="settings-check" title={s.enabled ? "Disable" : "Enable"}>
                          <input type="checkbox" checked={s.enabled} onChange={() => toggleEnabled(idx)} />
                          <span>Active</span>
                        </label>
                        <button
                          type="button"
                          className="settings-btn settings-btn-small"
                          onClick={() => void probeServer(s.name)}
                          disabled={probes[s.name]?.busy || !s.enabled}
                          title={s.enabled ? "Connect and list tools" : "Enable the server first"}
                        >
                          {probes[s.name]?.busy ? "Testing…" : "Test"}
                        </button>
                        <button type="button" className="settings-btn settings-btn-small" onClick={() => openEdit(idx)}>Edit</button>
                        <button type="button" className="settings-btn settings-btn-small settings-btn-danger" onClick={() => removeServer(idx)} title="Remove">
                          <BanIcon />
                        </button>
                      </div>
                      {probes[s.name] && (
                        <div className="settings-mcp-probe">
                          {probes[s.name].error ? (
                            <span className="settings-mcp-probe-error">✗ {probes[s.name].error}</span>
                          ) : probes[s.name].busy ? (
                            <span className="settings-mcp-probe-pending">Connecting…</span>
                          ) : probes[s.name].result && !probes[s.name].result!.ok ? (
                            <span className="settings-mcp-probe-error">✗ {probes[s.name].result!.error || "Failed to connect"}</span>
                          ) : probes[s.name].result ? (
                            <>
                              <span className="settings-mcp-probe-ok">
                                ✓ {probes[s.name].result!.tools.length} tool{probes[s.name].result!.tools.length === 1 ? "" : "s"}
                                {probes[s.name].result!.cached ? " (cached)" : ""}
                              </span>
                              {probes[s.name].result!.tools.length > 0 && (
                                <div className="settings-mcp-probe-tools">
                                  {probes[s.name].result!.tools.map((tool) => (
                                    <label key={tool} className="settings-mcp-tool" title="Uncheck to hide this tool from the agent">
                                      <input
                                        type="checkbox"
                                        checked={s.allowedTools.length === 0 || s.allowedTools.includes(tool)}
                                        onChange={() => toggleServerTool(idx, tool)}
                                      />
                                      <span>{tool}</span>
                                    </label>
                                  ))}
                                </div>
                              )}
                            </>
                          ) : null}
                        </div>
                      )}
                    </div>
                  ))}
                </div>
              )}
                </>
              )}

              <div className="settings-actions">
                <button type="button" className="settings-btn settings-btn-primary" onClick={() => void applyMcp()} disabled={saving}>
                  Apply
                </button>
                <button type="button" className="settings-btn settings-btn-danger" onClick={() => void resetMcp()}>
                  Reset
                </button>
              </div>
            </div>
          )}

          {activeSection === "agent" && (
            <div className="settings-panel">
              <h3 className="settings-panel-heading">Agent behavior</h3>
              <p className="settings-hint">
                Controls how the coding agent executes tasks. These match the toggles in the chat composer
                and are applied to the next task it runs.
              </p>

              <div className="settings-field">
                <label className="settings-label" htmlFor="setting-verify">
                  <label className="settings-check settings-check-inline">
                    <input
                      type="checkbox"
                      id="setting-verify"
                      checked={verify}
                      onChange={(e) => setVerify(e.target.checked)}
                    />
                    <span>Verify after edits</span>
                  </label>
                </label>
                <p className="settings-hint">
                  After making changes, the agent runs tests/typecheck (<code>tsc --noEmit</code>, pytest,
                  cargo check, …) to confirm nothing is broken before declaring the task complete.
                </p>
              </div>

              <div className="settings-field">
                <label className="settings-label" htmlFor="setting-yolo">
                  <label className="settings-check settings-check-inline">
                    <input
                      type="checkbox"
                      id="setting-yolo"
                      checked={yolo}
                      onChange={(e) => setYolo(e.target.checked)}
                    />
                    <span>YOLO mode</span>
                  </label>
                </label>
                <p className="settings-hint">
                  Auto-approve routine shell commands (tests, builds, inspections) without asking. Commands
                  that modify the system — installing packages, running servers, deleting files — stay in the
                  red zone and always require confirmation. Session-only; it re-enables the composer's YOLO toggle.
                </p>
              </div>

              <div className="settings-field">
                <label className="settings-label" htmlFor="setting-max-steps">Max steps per task</label>
                <div className="settings-slider-row">
                  <input
                    type="range"
                    id="setting-max-steps"
                    min="1"
                    max="20"
                    step="1"
                    value={maxSteps}
                    onChange={(e) => setMaxSteps(Number(e.target.value))}
                  />
                  <span className="settings-slider-value">{maxSteps}</span>
                </div>
                <p className="settings-hint">
                  Upper bound on the number of tool calls a single task may make before the agent reports
                  back. Higher values let long-running tasks finish but cost more time and tokens.
                </p>
              </div>

              <div className="settings-field">
                <label className="settings-label" htmlFor="setting-token-budget">Token budget per task</label>
                <div className="settings-slider-row">
                  <input
                    type="range"
                    id="setting-token-budget"
                    min="0"
                    max="200000"
                    step="5000"
                    value={tokenBudget}
                    onChange={(e) => setTokenBudget(Number(e.target.value))}
                  />
                  <span className="settings-slider-value">
                    {tokenBudget === 0 ? "Off" : tokenBudget.toLocaleString()}
                  </span>
                </div>
                <p className="settings-hint">
                  Total input+output token cap for a single agent run. At 0 there is no cap, and the run stops
                  once it reaches the budget instead of continuing into more steps.
                </p>
              </div>

              <div className="settings-field">
                <label className="settings-label" htmlFor="setting-decompose">
                  <label className="settings-check settings-check-inline">
                    <input
                      type="checkbox"
                      id="setting-decompose"
                      checked={decompose}
                      onChange={(e) => setDecompose(e.target.checked)}
                    />
                    <span>Decompose the goal first</span>
                  </label>
                </label>
                <p className="settings-hint">
                  Runs a goal-search phase that lists the ways to read the request before committing to one
                  and executing it. Slower, but less likely to head down the wrong path on vague tasks.
                </p>
              </div>

              <div className="settings-field">
                <label className="settings-label" htmlFor="setting-prefer-chat-complete">
                  <label className="settings-check settings-check-inline">
                    <input
                      type="checkbox"
                      id="setting-prefer-chat-complete"
                      checked={preferChatComplete}
                      onChange={(e) => setPreferChatComplete(e.target.checked)}
                    />
                    <span>Ask uses chat completion (faster, no tools)</span>
                  </label>
                </label>
                <p className="settings-hint">
                  When enabled, Ask mode sends a single chat-completion turn instead of starting the agent
                  loop: lower latency and no tools can run, but the model cannot search or read files.
                  Off by default.
                </p>
              </div>
            </div>
          )}

          {activeSection === "about" && (
            <div className="settings-panel">
              <h3 className="settings-panel-heading">About</h3>
              <div className="settings-about-block">
                <div className="settings-about-logo">AI</div>
                <div className="settings-about-name">AI Editor</div>
                <div className="settings-about-version">
                  Version {appVersion}
                  {isTauriRuntime() ? "" : " · dev preview (browser)"}
                </div>
              </div>
              <p className="settings-hint">
                A local-first coding agent that runs fully on your machine — models, edits, and execution all
                stay local. Pick a model on the Models page, then chat on the Home page to build.
              </p>
              <div className="settings-field">
                <label className="settings-label">System info</label>
                {systemInfo ? (
                  <div className="settings-about-meta">
                    <span>Cores: {systemInfo.cores}</span>
                    <span>RAM: {systemInfo.totalRamGb.toFixed(1)} GB total, {systemInfo.availableRamGb.toFixed(1)} GB free</span>
                    <span>GPU: {systemInfo.gpu ? `${systemInfo.gpu.name}${systemInfo.gpu.vramGb > 0 ? ` (${systemInfo.gpu.vramGb} GB)` : ""}` : "—"} {systemInfo.gpuEnabled ? "(enabled)" : "(off)"}</span>
                  </div>
                ) : (
                  <p className="settings-hint">Run the app natively to see live system details.</p>
                )}
              </div>
              <div className="settings-field">
                <label className="settings-label">Open project</label>
                <div className="settings-about-project" title={projectPath || undefined}>
                  {projectPath || "No project open"}
                </div>
              </div>
            </div>
          )}
        </div>
      </div>
    </div>
  );
}

const LEGACY_LOAD_SYNC: Array<[keyof LocalModelConfig, keyof LlmSettings]> = [
  ["contextSize", "contextSize"],
  ["nThreads", "nThreads"],
  ["nGpuLayers", "nGpuLayers"],
];

interface LFieldProps {
  id: string;
  label: string;
  hint?: string;
  badge?: string;
  disabled?: boolean;
}

function LNum({
  id,
  label,
  hint,
  badge,
  disabled,
  value,
  onChange,
  min,
  max,
  step,
  placeholder,
}: LFieldProps & {
  value: number | undefined;
  onChange: (v: number | undefined) => void;
  min?: number;
  max?: number;
  step?: number;
  placeholder?: string;
}) {
  return (
    <div className="settings-llm-item">
      <label className="settings-llm-label" htmlFor={id}>
        {label}
        {badge && <span className="settings-exp-badge">{badge}</span>}
      </label>
      <input
        type="number"
        id={id}
        className="settings-input settings-input-num"
        min={min}
        max={max}
        step={step}
        placeholder={placeholder}
        disabled={disabled}
        value={value ?? ""}
        onChange={(e) => {
          const raw = e.target.value;
          if (raw.trim() === "") {
            onChange(undefined);
          } else {
            const num = Number(raw);
            onChange(Number.isNaN(num) ? undefined : num);
          }
        }}
      />
      {hint && <span className="settings-llm-default">{hint}</span>}
    </div>
  );
}

export default SettingsPage;
