import { api } from "./ipc";
import { catalogKind } from "./providers";

export async function syncRemoteProviderToRegistry(opts: {
  presetId: string;
  displayName: string;
  baseUrl: string;
  apiKey: string;
  model: string;
}): Promise<void> {
  const id = opts.presetId === "custom" ? `custom-${opts.model || "default"}` : opts.presetId;
  const kind = catalogKind(opts.presetId);
  await api.providersUpsert({
    id,
    name: opts.displayName,
    kind,
    baseUrl: opts.baseUrl.trim() || undefined,
    apiKey: opts.apiKey.trim() || undefined,
    model: opts.model.trim(),
    roles: ["planner", "editor", "autocomplete"],
  });
  await api.providersSetRole("editor", id);
  await api.providersSetRole("planner", id);
}
