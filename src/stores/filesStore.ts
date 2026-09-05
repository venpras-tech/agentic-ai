import { create } from "zustand";
import type { OpenFile } from "../types";

interface FilesState {
  /** Open editor tabs (dirty state, path, content). */
  files: OpenFile[];
  /** Id of the focused tab (OpenFile.id). */
  activeKey: string | null;
  /** Open a discovered file: no-op if already open (just focuses it). */
  openFile: (file: OpenFile) => void;
  /** Create an untitled buffer. */
  newFile: (file: OpenFile) => void;
  /** Close a tab, moving focus to a sensible neighbor. */
  closeFile: (id: string) => void;
  /** Update a buffer's content (dirty if it changed). */
  updateContent: (id: string, content: string) => void;
  /** Apply an agent/disk update that is authoritative → buffer becomes clean. */
  syncSaved: (id: string, content: string) => void;
  /** Attach a persisted path+name to a previously-untitled buffer. */
  markSavedAs: (id: string, path: string, name: string) => void;
  /** Mark a buffer clean (after a successful save). */
  markSaved: (id: string) => void;
  /** Focus a tab by id. */
  setActive: (id: string | null) => void;
  /** Replace the whole tab set (workspace load / file-list sync). */
  setFiles: (files: OpenFile[], activeKey: string | null) => void;
}

export const useFilesStore = create<FilesState>((set, get) => ({
  files: [],
  activeKey: null,
  openFile: (file) => {
    if (get().files.some((f) => f.id === file.id)) {
      set({ activeKey: file.id });
      return;
    }
    set({ files: [...get().files, file], activeKey: file.id });
  },
  newFile: (file) => set({ files: [...get().files, file], activeKey: file.id }),
  closeFile: (id) =>
    set((state) => {
      const idx = state.files.findIndex((f) => f.id === id);
      const next = state.files.filter((f) => f.id !== id);
      let activeKey = state.activeKey;
      if (activeKey === id) {
        const neighbor = idx > 0 ? next[idx - 1] : next[idx];
        activeKey = neighbor ? neighbor.id : null;
      }
      return { files: next, activeKey };
    }),
  updateContent: (id, content) =>
    set((state) => ({
      files: state.files.map((f) =>
        f.id === id ? { ...f, content, saved: f.saved && f.content === content } : f,
      ),
    })),
  syncSaved: (id, content) =>
    set((state) => ({
      files: state.files.map((f) => (f.id === id ? { ...f, content, saved: true } : f)),
    })),
  markSavedAs: (id, path, name) =>
    set((state) => ({
      files: state.files.map((f) => (f.id === id ? { ...f, path, name, saved: true } : f)),
    })),
  markSaved: (id) =>
    set((state) => ({
      files: state.files.map((f) => (f.id === id ? { ...f, saved: true } : f)),
    })),
  setActive: (id) => set({ activeKey: id }),
  setFiles: (files, activeKey) => set({ files, activeKey }),
}));

export function resetFilesStoreForTests() {
  useFilesStore.setState({ files: [], activeKey: null });
}

/** Live snapshot of files, readable outside React (avoids stale closures). */
export function currentOpenFiles(): OpenFile[] {
  return useFilesStore.getState().files;
}