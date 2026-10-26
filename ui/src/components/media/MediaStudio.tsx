import { useCallback, useEffect, useState } from "react";
import { convertFileSrc } from "@tauri-apps/api/core";
import { api, isTauriRuntime, type MediaEngineStatus } from "../../lib/ipc";
import "./MediaStudio.css";

type StudioMode = "image" | "video";

const IMAGE_SIZES = [
  { label: "1024×1024", w: 1024, h: 1024 },
  { label: "1024×768", w: 1024, h: 768 },
  { label: "768×1024", w: 768, h: 1024 },
  { label: "832×480", w: 832, h: 480 },
];

const VIDEO_SIZES = [
  { label: "832×480", w: 832, h: 480 },
  { label: "640×360", w: 640, h: 360 },
];

function MediaStudio({ mode }: { mode: StudioMode }) {
  const [prompt, setPrompt] = useState("");
  const [width, setWidth] = useState(mode === "image" ? 1024 : 832);
  const [height, setHeight] = useState(mode === "image" ? 1024 : 480);
  const [duration, setDuration] = useState(4);
  const [genMode, setGenMode] = useState<"cloud" | "local">("cloud");
  const [busy, setBusy] = useState(false);
  const [status, setStatus] = useState<MediaEngineStatus | null>(null);
  const [message, setMessage] = useState<{ text: string; error?: boolean } | null>(null);
  const [previewPath, setPreviewPath] = useState<string | null>(null);
  const [videoJob, setVideoJob] = useState<string | null>(null);

  const refreshStatus = useCallback(() => {
    if (!isTauriRuntime()) return;
    void api.mediaEngineStatus().then(setStatus).catch(() => {});
  }, []);

  useEffect(() => {
    refreshStatus();
  }, [refreshStatus]);

  const generate = async () => {
    const t = prompt.trim();
    if (!t) {
      setMessage({ text: "Enter a prompt first.", error: true });
      return;
    }
    setBusy(true);
    setMessage(null);
    setPreviewPath(null);
    setVideoJob(null);
    try {
      if (mode === "image") {
        const res = await api.mediaGenerateImage({
          prompt: t,
          width,
          height,
          mode: genMode,
        });
        setPreviewPath(res.savedPath);
        setMessage({
          text: res.revisedPrompt
            ? `Saved · revised prompt: ${res.revisedPrompt}`
            : `Image saved (${res.provider}).`,
        });
      } else {
        const res = await api.mediaGenerateVideo({
          prompt: t,
          width,
          height,
          durationSecs: duration,
        });
        setVideoJob(res.jobId);
        setMessage({ text: res.message });
      }
    } catch (err) {
      setMessage({ text: err instanceof Error ? err.message : String(err), error: true });
    } finally {
      setBusy(false);
    }
  };

  const sizes = mode === "image" ? IMAGE_SIZES : VIDEO_SIZES;

  return (
    <div className="media-studio">
      <header className="media-studio-header">
        <h1>{mode === "image" ? "Images" : "Video"}</h1>
        <p>
          {mode === "image"
            ? "Generate images from a prompt — cloud providers (OpenAI-compatible) today; local checkpoints via the media engine paths in llama.cpp settings."
            : "Describe motion and length — local Wan/LTX-style models use the media engine (configure in Settings → llama.cpp)."}
        </p>
        {status ? <p className="media-studio-status">{status.message}</p> : null}
      </header>

      <div className="media-studio-layout">
        <section className="media-studio-controls">
          <label className="media-label">
            Prompt
            <textarea
              className="media-prompt"
              rows={5}
              value={prompt}
              placeholder={
                mode === "image"
                  ? "A studio photo of a red sports car, soft lighting…"
                  : "Camera slowly pans across a rainy city street at night…"
              }
              onChange={(e) => setPrompt(e.target.value)}
            />
          </label>

          <div className="media-row">
            <label className="media-label">
              Size
              <select
                className="media-select"
                value={`${width}x${height}`}
                onChange={(e) => {
                  const [w, h] = e.target.value.split("x").map(Number);
                  setWidth(w);
                  setHeight(h);
                }}
              >
                {sizes.map((s) => (
                  <option key={s.label} value={`${s.w}x${s.h}`}>{s.label}</option>
                ))}
              </select>
            </label>
            {mode === "video" ? (
              <label className="media-label">
                Duration (s)
                <input
                  type="number"
                  className="media-select"
                  min={2}
                  max={16}
                  value={duration}
                  onChange={(e) => setDuration(Number(e.target.value) || 4)}
                />
              </label>
            ) : (
              <label className="media-label">
                Source
                <select
                  className="media-select"
                  value={genMode}
                  onChange={(e) => setGenMode(e.target.value as "cloud" | "local")}
                >
                  <option value="cloud">Cloud (active API provider)</option>
                  <option value="local">Local engine (paths in settings)</option>
                </select>
              </label>
            )}
          </div>

          <button type="button" className="media-generate-btn" disabled={busy} onClick={() => void generate()}>
            {busy ? "Generating…" : "Generate"}
          </button>

          {message ? (
            <p className={`media-msg${message.error ? " error" : ""}`} role="status">{message.text}</p>
          ) : null}
        </section>

        <section className="media-studio-preview" aria-label="Preview">
          {mode === "image" && previewPath && isTauriRuntime() ? (
            <img src={convertFileSrc(previewPath)} alt="Generated" className="media-preview-img" />
          ) : mode === "video" && videoJob ? (
            <div className="media-video-placeholder">
              <p>Job {videoJob}</p>
              <p className="media-muted">Local video pipeline will render here when the media engine is installed.</p>
            </div>
          ) : (
            <div className="media-empty-preview">
              <p>Your {mode === "image" ? "image" : "clip"} will appear here.</p>
            </div>
          )}
        </section>
      </div>
    </div>
  );
}

export default MediaStudio;
