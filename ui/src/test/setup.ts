import { vi } from "vitest";

/**
 * `tauriInvoke` refuses to call anything unless `window.__TAURI_INTERNALS__`
 * exists, so unit tests need a minimal stand-in for the desktop shell. The
 * per-test `invoke` mock is installed on the same object.
 */
const internals = { invoke: vi.fn(async () => undefined) };

if (typeof globalThis.window === "undefined") {
  (globalThis as unknown as { window: unknown }).window = globalThis;
}
(globalThis as unknown as { window: { __TAURI_INTERNALS__: unknown } }).window.__TAURI_INTERNALS__ =
  internals;

export { internals };
