import { beforeEach, describe, expect, it } from "vitest";
import {
  currentOpenFiles,
  resetFilesStoreForTests,
  useFilesStore,
} from "./filesStore";
import type { OpenFile } from "../types";

const file = (id: string): OpenFile => ({
  id,
  path: id.includes("new") ? null : `/m/${id}.rs`,
  name: `${id}.rs`,
  content: "x",
  saved: true,
});

describe("filesStore", () => {
  beforeEach(() => {
    resetFilesStoreForTests();
  });

  it("opens a file and focuses it; no duplicate open", () => {
    useFilesStore.getState().openFile(file("a"));
    useFilesStore.getState().openFile(file("b"));
    expect(currentOpenFiles()).toHaveLength(2);
    useFilesStore.getState().openFile(file("a"));
    expect(currentOpenFiles()).toHaveLength(2);
    expect(useFilesStore.getState().activeKey).toBe("a");
  });

  it("closes a file and moves focus to a neighbor", () => {
    useFilesStore.getState().openFile(file("a"));
    useFilesStore.getState().openFile(file("b"));
    useFilesStore.getState().openFile(file("c"));
    useFilesStore.getState().closeFile("b");
    expect(useFilesStore.getState().activeKey).toBe("c");
    useFilesStore.getState().closeFile("c");
    expect(useFilesStore.getState().activeKey).toBe("a");
    useFilesStore.getState().closeFile("a");
    expect(useFilesStore.getState().activeKey).toBeNull();
    expect(currentOpenFiles()).toHaveLength(0);
  });

  it("tracks dirty state on content update", () => {
    useFilesStore.getState().openFile(file("a"));
    useFilesStore.getState().updateContent("a", "y");
    const f = currentOpenFiles().find((x) => x.id === "a")!;
    expect(f.content).toBe("y");
    expect(f.saved).toBe(false);
    useFilesStore.getState().markSaved("a");
    expect(currentOpenFiles().find((x) => x.id === "a")!.saved).toBe(true);
  });

  it("syncSaved marks agent/disk updates authoritative (clean)", () => {
    useFilesStore.getState().openFile(file("a"));
    useFilesStore.getState().updateContent("a", "y"); // dirty
    useFilesStore.getState().syncSaved("a", "z"); // agent wrote z to disk
    const f = currentOpenFiles().find((x) => x.id === "a")!;
    expect(f.content).toBe("z");
    expect(f.saved).toBe(true);
  });

  it("markSavedAs attaches a persisted path to an untitled buffer", () => {
    useFilesStore.getState().newFile(file("new:1"));
    const uid = currentOpenFiles()[0].id;
    useFilesStore.getState().markSavedAs(uid, "/m/real.rs", "real.rs");
    const f = currentOpenFiles()[0];
    expect(f.path).toBe("/m/real.rs");
    expect(f.name).toBe("real.rs");
    expect(f.saved).toBe(true);
  });

  it("newFile creates an untitled buffer and focuses it", () => {
    useFilesStore.getState().newFile(file("new:1"));
    useFilesStore.getState().newFile(file("new:2"));
    expect(currentOpenFiles()).toHaveLength(2);
    expect(useFilesStore.getState().activeKey).toBe("new:2");
  });
});