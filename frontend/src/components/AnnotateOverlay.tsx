import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import "./AnnotateOverlay.css";

/**
 * Visual Annotate overlay — the desktop-native answer to Agentation.
 *
 * Lives in the borderless full-screen `annotate` window (`#/annotate`). The
 * Rust side (`annotate::begin`) freeze-captures the monitor under the cursor,
 * sizes this window to cover it exactly, and stashes the screenshot; this
 * component fetches it, lets the user drop numbered pins + type feedback,
 * then composites the pins onto the image and sends it to a Groq vision model
 * via `run_annotation`. The model's structured markdown is copied to the
 * clipboard (for pasting into Claude Code / Cursor) and shown here.
 *
 * Flow states: loading → annotating → generating → result | error.
 */

interface Pin {
  n: number;
  xPct: number; // 0..1 across the image width
  yPct: number; // 0..1 down the image height
  note: string;
}

type Phase = "loading" | "annotating" | "generating" | "result" | "error";

/** Longest edge (px) of the image sent to the model. Keeps the base64
 * payload comfortably under Groq's per-image limit and speeds the call up;
 * a screenshot downscaled to this is still legible to a vision model. */
const MAX_SEND_EDGE = 1400;

export function AnnotateOverlay() {
  const [phase, setPhase] = useState<Phase>("loading");
  const [imageUrl, setImageUrl] = useState<string | null>(null);
  const [pins, setPins] = useState<Pin[]>([]);
  const [result, setResult] = useState<string>("");
  const [error, setError] = useState<string>("");
  const [copied, setCopied] = useState(false);
  const nextN = useRef(1);
  const imgRef = useRef<HTMLImageElement | null>(null);
  const noteRefs = useRef<Record<number, HTMLInputElement | null>>({});

  // The overlay window is opaque (it shows a screenshot), but Shell.css paints
  // --pf-bg-deep on html/body/#root; strip it so nothing flashes at the edges.
  useEffect(() => {
    document.body.classList.add("annotate-route");
    return () => document.body.classList.remove("annotate-route");
  }, []);

  // Pull the freshly-captured screenshot from the backend on mount.
  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const url = await invoke<string | null>("get_annotation_capture");
        if (cancelled) return;
        if (!url) {
          setError("No screenshot was captured. Close this and try again.");
          setPhase("error");
          return;
        }
        setImageUrl(url);
        setPhase("annotating");
      } catch (e) {
        if (cancelled) return;
        setError(String(e));
        setPhase("error");
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  const close = useCallback(async () => {
    try {
      await invoke("close_annotation");
    } catch {
      /* window teardown is best-effort */
    }
  }, []);

  // Escape cancels the whole session (unless we're mid-generation).
  useEffect(() => {
    function onKey(e: KeyboardEvent) {
      if (e.key === "Escape" && phase !== "generating") {
        e.preventDefault();
        void close();
      }
    }
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [phase, close]);

  function addPin(e: React.MouseEvent<HTMLDivElement>) {
    if (phase !== "annotating") return;
    const img = imgRef.current;
    if (!img) return;
    const rect = img.getBoundingClientRect();
    const xPct = (e.clientX - rect.left) / rect.width;
    const yPct = (e.clientY - rect.top) / rect.height;
    if (xPct < 0 || xPct > 1 || yPct < 0 || yPct > 1) return;
    const n = nextN.current++;
    setPins((prev) => [...prev, { n, xPct, yPct, note: "" }]);
    // Focus the new pin's note field on the next tick.
    requestAnimationFrame(() => noteRefs.current[n]?.focus());
  }

  function updateNote(n: number, note: string) {
    setPins((prev) => prev.map((p) => (p.n === n ? { ...p, note } : p)));
  }

  function removePin(n: number) {
    setPins((prev) => prev.filter((p) => p.n !== n));
  }

  /** Draw the screenshot + numbered pin markers onto a downscaled canvas and
   * return a JPEG data URL. Drawing the markers into the pixels is what lets
   * the vision model ground each note to a location. */
  async function compositeAnnotatedImage(): Promise<string> {
    const img = imgRef.current;
    if (!img) throw new Error("image not ready");
    const natW = img.naturalWidth || img.width;
    const natH = img.naturalHeight || img.height;
    const scale = Math.min(1, MAX_SEND_EDGE / Math.max(natW, natH));
    const w = Math.round(natW * scale);
    const h = Math.round(natH * scale);

    const canvas = document.createElement("canvas");
    canvas.width = w;
    canvas.height = h;
    const ctx = canvas.getContext("2d");
    if (!ctx) throw new Error("no 2d canvas context");
    ctx.drawImage(img, 0, 0, w, h);

    const r = Math.max(13, Math.round(Math.min(w, h) * 0.016));
    for (const p of pins) {
      const cx = p.xPct * w;
      const cy = p.yPct * h;
      // Marker disc
      ctx.beginPath();
      ctx.arc(cx, cy, r, 0, Math.PI * 2);
      ctx.fillStyle = "#ff7a4d";
      ctx.strokeStyle = "#ffffff";
      ctx.lineWidth = Math.max(2, r * 0.18);
      ctx.fill();
      ctx.stroke();
      // Number
      ctx.fillStyle = "#1a1a18";
      ctx.font = `700 ${Math.round(r * 1.25)}px "Inter Tight", system-ui, sans-serif`;
      ctx.textAlign = "center";
      ctx.textBaseline = "middle";
      ctx.fillText(String(p.n), cx, cy + r * 0.05);
    }
    return canvas.toDataURL("image/jpeg", 0.85);
  }

  async function generate() {
    if (pins.length === 0) return;
    setPhase("generating");
    setError("");
    try {
      const image_data_url = await compositeAnnotatedImage();
      const payload = pins
        .slice()
        .sort((a, b) => a.n - b.n)
        .map((p) => ({ n: p.n, note: p.note }));
      const md = await invoke<string>("run_annotation", {
        imageDataUrl: image_data_url,
        pins: payload,
      });
      setResult(md);
      setCopied(true); // backend already copied it to the clipboard
      setPhase("result");
    } catch (e) {
      setError(String(e));
      setPhase("annotating");
    }
  }

  async function copyResult() {
    try {
      await invoke("copy_text", { text: result });
      setCopied(true);
    } catch {
      // Fall back to the web clipboard API if the helper command is absent.
      try {
        await navigator.clipboard.writeText(result);
        setCopied(true);
      } catch {
        /* ignore */
      }
    }
  }

  if (phase === "loading") {
    return (
      <div className="an-root an-centered">
        <div className="an-spinner" aria-hidden />
        <span className="an-loading-text">Capturing your screen…</span>
      </div>
    );
  }

  if (phase === "error") {
    return (
      <div className="an-root an-centered">
        <div className="an-panel an-error-panel">
          <div className="an-panel-title">Couldn’t start annotate</div>
          <p className="an-error-msg">{error}</p>
          <div className="an-actions">
            <button className="an-btn an-btn-primary" onClick={() => void close()}>
              Close
            </button>
          </div>
        </div>
      </div>
    );
  }

  return (
    <div className="an-root">
      {/* The frozen screenshot fills the window; clicking it drops a pin. */}
      <div className="an-canvas" onClick={addPin}>
        {imageUrl && (
          <img
            ref={imgRef}
            src={imageUrl}
            className="an-shot"
            alt="Captured screen"
            draggable={false}
          />
        )}
        {pins.map((p) => (
          <div
            key={p.n}
            className="an-pin"
            style={{ left: `${p.xPct * 100}%`, top: `${p.yPct * 100}%` }}
          >
            <span className="an-pin-dot">{p.n}</span>
          </div>
        ))}

        {phase === "generating" && (
          <div className="an-scrim">
            <div className="an-spinner" aria-hidden />
            <span className="an-loading-text">Reading your screen &amp; mapping to source…</span>
          </div>
        )}
      </div>

      {/* Control panel. Floats bottom-center over the screenshot. */}
      {phase !== "result" && (
        <div className="an-panel an-dock">
          <div className="an-panel-head">
            <span className="an-brand">Annotate</span>
            <span className="an-hint">
              Click anywhere on the screen to drop a pin, then describe the change.
            </span>
          </div>

          {pins.length === 0 ? (
            <div className="an-empty">No pins yet — click a UI element above.</div>
          ) : (
            <div className="an-pinlist">
              {pins
                .slice()
                .sort((a, b) => a.n - b.n)
                .map((p) => (
                  <div key={p.n} className="an-pinrow">
                    <span className="an-pinrow-n">{p.n}</span>
                    <input
                      ref={(el) => {
                        noteRefs.current[p.n] = el;
                      }}
                      className="an-note"
                      value={p.note}
                      placeholder="What should change here?"
                      onChange={(e) => updateNote(p.n, e.target.value)}
                      onKeyDown={(e) => {
                        if (e.key === "Enter") {
                          e.preventDefault();
                          void generate();
                        }
                      }}
                    />
                    <button
                      className="an-pinrow-x"
                      onClick={() => removePin(p.n)}
                      aria-label={`Remove pin ${p.n}`}
                    >
                      ✕
                    </button>
                  </div>
                ))}
            </div>
          )}

          {error && <p className="an-error-inline">{error}</p>}

          <div className="an-actions">
            <button className="an-btn" onClick={() => void close()} disabled={phase === "generating"}>
              Cancel <kbd>Esc</kbd>
            </button>
            <button
              className="an-btn an-btn-primary"
              onClick={() => void generate()}
              disabled={pins.length === 0 || phase === "generating"}
            >
              {phase === "generating"
                ? "Generating…"
                : `Generate context${pins.length ? ` · ${pins.length}` : ""}`}
            </button>
          </div>
        </div>
      )}

      {/* Result panel replaces the dock once we have output. */}
      {phase === "result" && (
        <div className="an-panel an-result-panel">
          <div className="an-panel-head">
            <span className="an-brand">Context ready</span>
            <span className="an-hint">
              {copied ? "Copied to clipboard — paste into your agent (Ctrl+V)." : "Copy and paste into your agent."}
            </span>
          </div>
          <pre className="an-result">{result}</pre>
          <div className="an-actions">
            <button className="an-btn" onClick={() => void copyResult()}>
              {copied ? "Copied ✓" : "Copy again"}
            </button>
            <button className="an-btn an-btn-primary" onClick={() => void close()}>
              Done
            </button>
          </div>
        </div>
      )}
    </div>
  );
}
