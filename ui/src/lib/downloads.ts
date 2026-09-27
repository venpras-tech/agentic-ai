import { useCallback, useEffect, useRef, useState } from "react";
import { api } from "./ipc";

export interface DownloadProgress {
  repoId: string;
  file: string;
  receivedBytes: number;
  totalBytes: number;
  done: boolean;
  cancelled?: boolean;
  paused?: boolean;
  error?: string;
  /** Bytes per second, sampled from a rolling window. 0 while paused. */
  speedBps?: number;
  /** Seconds remaining at the current speed; undefined until speed is known. */
  etaSeconds?: number;
  /** Absolute on-disk path once the download finishes. */
  path?: string;
}

export const KB = 1024;
export const MB = KB * 1024;
export const GB = MB * 1024;

export function formatBytes(bytes?: number | null): string {
  if (!bytes) return "—";
  if (bytes >= GB) return `${(bytes / GB).toFixed(1)} GB`;
  if (bytes >= MB) return `${(bytes / MB).toFixed(0)} MB`;
  if (bytes >= KB) return `${(bytes / KB).toFixed(0)} KB`;
  return `${bytes} B`;
}

export function formatSpeed(bps?: number): string {
  if (!bps || bps <= 0) return "";
  if (bps >= MB) return `${(bps / MB).toFixed(1)} MB/s`;
  if (bps >= KB) return `${(bps / KB).toFixed(0)} KB/s`;
  return `${Math.round(bps)} B/s`;
}

export function formatEta(seconds?: number): string {
  if (!seconds || !isFinite(seconds) || seconds < 0) return "";
  const s = Math.round(seconds);
  if (s < 60) return `${s}s left`;
  if (s < 3600) return `${Math.floor(s / 60)}m ${s % 60}s left`;
  return `${(s / 3600).toFixed(1)}h left`;
}

/** Parse the quantization tag from a GGUF file name, e.g. `Q4_K_M` from `model-Q4_K_M.gguf`. */
export function parseQuant(fileName: string): string {
  const m = fileName.match(/Q(\d+[_A-Z0-9]*)/i);
  if (m) return `Q${m[1].toUpperCase()}`;
  if (/f16/i.test(fileName)) return "F16";
  if (/f32/i.test(fileName)) return "F32";
  if (/i(\d+)/i.test(fileName)) return `I${RegExp.$1}`;
  return "—";
}

/**
 * Rough RAM estimate in GB for a GGUF model (CPU, llama.cpp).
 * file_bytes is the on-disk size; model cores add ~5-10 % overhead.
 * Context overhead assumed ~0.8 GB for 4k ctx, ~1.5 GB for 8k, more for larger.
 */
export function estimateRamGb(fileBytes: number, contextTokens = 8192): number {
  if (!fileBytes) return 0;
  const modelGb = (fileBytes * 1.12) / GB;
  const ctxGb = contextTokens <= 4096 ? 0.8 : contextTokens <= 8192 ? 1.5 : contextTokens <= 32192 ? 3.0 : 6.0;
  return modelGb + ctxGb;
}

/**
 * Fit verdict given available RAM (GB) and required RAM (GB).
 * Returns one of: "fits" | "tight" | "no" | "unknown"
 */
export function fitStatus(availableGb: number, requiredGb: number): "fits" | "tight" | "no" | "unknown" {
  if (!requiredGb || !availableGb) return "unknown";
  if (availableGb >= requiredGb * 1.25) return "fits";
  if (availableGb >= requiredGb * 0.95) return "tight";
  return "no";
}

/** Active download map key */
export function dlKey(repoId: string, file: string) {
  return `${repoId}::${file}`;
}

interface SpeedSample {
  t: number;
  bytes: number;
}

export function useDownloadManager() {
  const [downloads, setDownloads] = useState<Record<string, DownloadProgress>>({});
  const mounted = useRef(true);
  const samples = useRef<Record<string, SpeedSample>>({});

  // Hydrate from backend on mount
  useEffect(() => {
    mounted.current = true;
    void api.hfListActive().then((list) => {
      if (!mounted.current || !list) return;
      setDownloads((prev) => {
        const next = { ...prev };
        for (const { repoId, fileName, paused } of list) {
          const key = dlKey(repoId, fileName);
          next[key] = { repoId, file: fileName, receivedBytes: 0, totalBytes: 0, done: false, paused };
        }
        return next;
      });
    }).catch(() => {});

    let unlisten: (() => void) | null = null;
    api.hfDownloadProgress((data) => {
      const repoId = String(data.repoId || "");
      const file = String(data.file || "");
      const key = dlKey(repoId, file);
      const done = Boolean(data.done);
      const received = Number(data.receivedBytes ?? 0);
      const total = Number(data.totalBytes ?? 0);
      const now = Date.now();

      const speedBps = (() => {
        if (data.paused) return 0;
        const prev = samples.current[key];
        const dt = prev ? (now - prev.t) : 0;
        if (prev && dt > 200 && dt < 15000 && received >= prev.bytes && !done) {
          return ((received - prev.bytes) * 1000) / dt;
        }
        return undefined;
      })();

      if (done || data.cancelled || data.paused) delete samples.current[key];
      else if (received > 0) samples.current[key] = { t: now, bytes: received };

      const etaSeconds =
        speedBps && speedBps > 0 && total > received && !done
          ? (total - received) / speedBps
          : undefined;

      setDownloads((prev) => {
        const existing = prev[key] || { repoId, file, receivedBytes: 0, totalBytes: 0, done: false };
        const next: DownloadProgress = {
          ...existing,
          receivedBytes: received || existing.receivedBytes,
          totalBytes: total || existing.totalBytes,
          done,
        };
        if (data.cancelled) next.cancelled = true;
        if (data.paused) next.paused = true;
        next.speedBps = speedBps;
        next.etaSeconds = etaSeconds;
        if (data.error) next.error = String(data.error);
        return { ...prev, [key]: next };
      });
    }).then((un) => { unlisten = un; }).catch(() => {});

    return () => {
      mounted.current = false;
      unlisten?.();
    };
  }, []);

  const download = useCallback(async (repoId: string, file: string) => {
    const key = dlKey(repoId, file);
    try {
      await api.hfDownloadModel(repoId, file);
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      setDownloads((prev) => {
        const existing = prev[key] || { repoId, file, receivedBytes: 0, totalBytes: 0, done: false };
        return { ...prev, [key]: { ...existing, error: msg, cancelled: false, paused: false } };
      });
      throw err;
    }
  }, []);

  const pause = useCallback(async (repoId: string, file: string) => {
    await api.hfPauseDownload(repoId, file);
  }, []);

  const resume = useCallback(async (repoId: string, file: string) => {
    await api.hfResumeDownload(repoId, file);
  }, []);

  const cancel = useCallback(async (repoId: string, file: string, deletePart = false) => {
    await api.hfCancelDownload(repoId, file, deletePart);
  }, []);

  /** Remove a finished/error entry from the notification strip. */
  const dismiss = useCallback((repoId: string, file: string) => {
    const key = dlKey(repoId, file);
    delete samples.current[key];
    setDownloads((prev) => {
      const next = { ...prev };
      delete next[key];
      return next;
    });
  }, []);

  /** Load a finished download into the local backend (llama.cpp). */
  const loadModel = useCallback(async (repoId: string, file: string, params?: Record<string, unknown>) => {
    const key = dlKey(repoId, file);
    const list = await api.listDownloadedModels();
    const model = list.find((m) => dlKey(m.repoId, m.fileName) === key);
    if (!model) throw new Error("Model file not found on disk.");
    return api.loadModelFromPath(model.path, params);
  }, []);

  const active = Object.values(downloads).filter((d) => !d.done && !d.cancelled && !d.error);
  const completed = Object.values(downloads).filter((d) => d.done);

  return { downloads, download, pause, resume, cancel, dismiss, loadModel, active, completed, activeCount: active.length };
}