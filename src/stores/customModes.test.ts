import { beforeEach, describe, expect, it, vi } from "vitest";

import { useCustomModes } from "./customModes";
import * as ipc from "../lib/ipc";

vi.mock("../lib/ipc", async (importOriginal) => {
  const actual = (await importOriginal()) as typeof ipc;
  return {
    ...actual,
    api: {
      modesLoad: vi.fn(),
      contextSetSystemPrompt: vi.fn().mockResolvedValue(undefined),
    },
    isTauriRuntime: vi.fn(() => true),
  };
});

const mockApi = ipc.api as unknown as {
  modesLoad: ReturnType<typeof vi.fn>;
  contextSetSystemPrompt: ReturnType<typeof vi.fn>;
};
const isTauriRuntime = ipc.isTauriRuntime as unknown as ReturnType<typeof vi.fn>;

const MODES = [
  { name: "explorer", description: "ro", systemPrompt: "You explore.", allowedTools: [] },
  { name: "fixer", description: "fix", systemPrompt: "You fix.", allowedTools: ["edit_file"] },
];

beforeEach(() => {
  useCustomModes.setState({ customModes: [], activeCustomMode: null, activeModeRef: null });
  mockApi.modesLoad.mockResolvedValue(MODES);
  mockApi.contextSetSystemPrompt.mockClear();
  isTauriRuntime.mockReturnValue(true);
});

describe("useCustomModes store", () => {
  it("loads modes for a workspace", async () => {
    await useCustomModes.getState().syncWithWorkspace("/proj");
    const s = useCustomModes.getState();
    expect(s.customModes).toEqual(MODES);
    expect(useCustomModes.getState().customModes).toHaveLength(2);
  });

  it("clears modes when workspace is null or not a Tauri runtime", async () => {
    isTauriRuntime.mockReturnValue(false);
    await useCustomModes.getState().syncWithWorkspace(null);
    expect(useCustomModes.getState().customModes).toEqual([]);
  });

  it("applyCustomMode sets active mode and system prompt", async () => {
    await useCustomModes.getState().syncWithWorkspace("/proj");
    await useCustomModes.getState().applyCustomMode("fixer");
    const s = useCustomModes.getState();
    expect(s.activeCustomMode).toBe("fixer");
    expect(s.activeModeRef?.name).toBe("fixer");
    expect(mockApi.contextSetSystemPrompt).toHaveBeenCalledTimes(1);
    expect(s.modeAwareSystemPrompt()).toContain("## Active mode: fixer");
    expect(s.modeAwareSystemPrompt()).toContain("You fix.");
  });

  it("applyCustomMode with unknown name clears active mode", async () => {
    await useCustomModes.getState().syncWithWorkspace("/proj");
    await useCustomModes.getState().applyCustomMode(null);
    const s = useCustomModes.getState();
    expect(s.activeCustomMode).toBeNull();
    expect(s.activeModeRef).toBeNull();
    expect(s.modeAwareSystemPrompt()).not.toContain("## Active mode");
  });

  it("drops an active mode that no longer exists after reload", async () => {
    await useCustomModes.getState().syncWithWorkspace("/proj");
    await useCustomModes.getState().applyCustomMode("fixer");
    mockApi.modesLoad.mockResolvedValue([MODES[0]]);
    await useCustomModes.getState().syncWithWorkspace("/proj");
    const s = useCustomModes.getState();
    expect(s.activeCustomMode).toBeNull();
    expect(s.activeModeRef).toBeNull();
    expect(s.customModes).toHaveLength(1);
  });
});