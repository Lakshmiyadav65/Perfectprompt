import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import "./MicOverlay.css";

/**
 * Mic overlay — the voice twin of AnnotateOverlay.
 *
 * Lives in the borderless, transparent, non-focusable `mic` window
 * (`#/mic`). The Rust side (`mic::begin`) shows this window and emits
 * `mic://start` when a push-to-talk chord is pressed; on release it emits
 * `mic://stop`. This component owns the actual audio capture via
 * `getUserMedia` + `MediaRecorder` (the browser already has a first-class
 * media stack, so we don't add a native audio dependency — same division of
 * labour as Annotate compositing pins in the frontend). When recording stops
 * it base64-encodes the clip and hands it to `transcribe_and_enhance`, which
 * runs Groq Whisper → the enhance/polish pipeline → paste at the cursor.
 *
 * The window is non-focusable so the user's target field keeps focus and the
 * synthetic paste lands there. Flow states: listening → transcribing → done |
 * error.
 */

type MicMode = "enhance" | "dictate";
type Phase = "listening" | "transcribing" | "done" | "error";

/** Preferred recording containers, best-first. Groq Whisper accepts all of
 * these; webm/opus is the MediaRecorder default on Chromium (WebView2). */
const MIME_CANDIDATES = [
  "audio/webm;codecs=opus",
  "audio/webm",
  "audio/ogg;codecs=opus",
  "audio/mp4",
];

function pickMime(): string {
  if (typeof MediaRecorder === "undefined") return "";
  for (const m of MIME_CANDIDATES) {
    try {
      if (MediaRecorder.isTypeSupported(m)) return m;
    } catch {
      /* isTypeSupported can throw on some engines — treat as unsupported */
    }
  }
  return "";
}

/** Read a Blob as bare base64 (no `data:` prefix) for the Rust command. */
function blobToBase64(blob: Blob): Promise<string> {
  return new Promise((resolve, reject) => {
    const fr = new FileReader();
    fr.onerror = () => reject(fr.error ?? new Error("audio read failed"));
    fr.onload = () => {
      const s = String(fr.result);
      const comma = s.indexOf(",");
      resolve(comma >= 0 ? s.slice(comma + 1) : s);
    };
    fr.readAsDataURL(blob);
  });
}

const MODE_LABEL: Record<MicMode, string> = {
  enhance: "Perfect prompt",
  dictate: "Dictation",
};

export function MicOverlay() {
  const [phase, setPhase] = useState<Phase>("listening");
  const [mode, setMode] = useState<MicMode>("enhance");
  const [error, setError] = useState<string>("");

  const recorderRef = useRef<MediaRecorder | null>(null);
  const streamRef = useRef<MediaStream | null>(null);
  const chunksRef = useRef<BlobPart[]>([]);
  const modeRef = useRef<MicMode>("enhance");
  const stopRequestedRef = useRef(false);
  const processingRef = useRef(false);

  // Shell.css paints --pf-bg-deep on html/body via the shared bundle; strip it
  // so the pill floats on true transparency (same trick as StatusIndicator).
  useEffect(() => {
    document.body.classList.add("mic-route");
    const prev = document.documentElement.style.background;
    document.documentElement.style.background = "transparent";
    return () => {
      document.body.classList.remove("mic-route");
      document.documentElement.style.background = prev;
    };
  }, []);

  const releaseStream = useCallback(() => {
    const s = streamRef.current;
    if (s) {
      // Stop the tracks so the OS mic-in-use indicator clears immediately —
      // we only hold the mic for the duration of the hold, not continuously.
      s.getTracks().forEach((t) => t.stop());
      streamRef.current = null;
    }
  }, []);

  const autoClose = useCallback((ms: number) => {
    window.setTimeout(() => {
      void invoke("close_mic").catch(() => {
        /* window teardown is best-effort */
      });
    }, ms);
  }, []);

  const handleStopped = useCallback(async () => {
    releaseStream();
    const rec = recorderRef.current;
    recorderRef.current = null;
    const type = rec?.mimeType || pickMime() || "audio/webm";
    const blob = new Blob(chunksRef.current, { type });
    chunksRef.current = [];

    // Too little audio to be real speech — an accidental tap.
    if (blob.size < 1500) {
      setError("Didn't catch that — hold the key and speak a moment longer.");
      setPhase("error");
      autoClose(2600);
      return;
    }

    processingRef.current = true;
    setPhase("transcribing");
    try {
      const base64 = await blobToBase64(blob);
      await invoke<string>("transcribe_and_enhance", {
        audioBase64: base64,
        mime: type,
        mode: modeRef.current,
      });
      setPhase("done");
      autoClose(900);
    } catch (err) {
      setError(errorMessage(err));
      setPhase("error");
      autoClose(4200);
    } finally {
      processingRef.current = false;
    }
  }, [autoClose, releaseStream]);

  const beginRecording = useCallback(
    async (m: MicMode) => {
      // Ignore a fresh start while a prior clip is still being transcribed.
      if (processingRef.current) return;

      setMode(m);
      modeRef.current = m;
      setError("");
      setPhase("listening");
      stopRequestedRef.current = false;
      chunksRef.current = [];

      const preferred = pickMime();
      try {
        const stream = await navigator.mediaDevices.getUserMedia({
          audio: { echoCancellation: true, noiseSuppression: true },
        });
        streamRef.current = stream;

        const rec = new MediaRecorder(
          stream,
          preferred ? { mimeType: preferred } : undefined,
        );
        recorderRef.current = rec;
        rec.ondataavailable = (ev) => {
          if (ev.data && ev.data.size > 0) chunksRef.current.push(ev.data);
        };
        rec.onstop = () => {
          void handleStopped();
        };
        rec.start();

        // Race: the user released the key while we were still acquiring the
        // mic. Honour the pending stop now that the recorder exists.
        if (stopRequestedRef.current) {
          try {
            rec.stop();
          } catch {
            /* already inactive */
          }
        }
      } catch (err) {
        releaseStream();
        setError(micErrorMessage(err));
        setPhase("error");
        autoClose(4500);
      }
    },
    [autoClose, handleStopped, releaseStream],
  );

  const endRecording = useCallback(() => {
    stopRequestedRef.current = true;
    const rec = recorderRef.current;
    if (rec && rec.state !== "inactive") {
      try {
        rec.stop(); // fires onstop → handleStopped
      } catch {
        /* already stopped */
      }
    }
  }, []);

  // Wire the backend push-to-talk events. Cleanup unlistens so StrictMode's
  // dev double-mount doesn't leave two subscriptions handling each event.
  useEffect(() => {
    let unStart: (() => void) | undefined;
    let unStop: (() => void) | undefined;
    let disposed = false;

    void (async () => {
      const u1 = await listen<{ mode?: MicMode }>("mic:start", (e) => {
        const m: MicMode = e.payload?.mode === "dictate" ? "dictate" : "enhance";
        void beginRecording(m);
      });
      const u2 = await listen("mic:stop", () => {
        endRecording();
      });
      if (disposed) {
        u1();
        u2();
        return;
      }
      unStart = u1;
      unStop = u2;
    })();

    return () => {
      disposed = true;
      unStart?.();
      unStop?.();
    };
  }, [beginRecording, endRecording]);

  // Manual controls for mouse users (the window is non-focusable but still
  // receives clicks): stop a recording, or dismiss an error.
  const onStopClick = () => endRecording();
  const onDismiss = () => void invoke("close_mic").catch(() => {});

  return (
    <div className={`mic-pill mic-${phase}`} role="status" aria-live="polite">
      {phase === "listening" && (
        <>
          <span className="mic-bars" aria-hidden="true">
            <i />
            <i />
            <i />
            <i />
          </span>
          <span className="mic-text">
            Listening<span className="mic-ellipsis" aria-hidden="true" />
            <span className="mic-mode-tag">{MODE_LABEL[mode]}</span>
          </span>
          <button
            type="button"
            className="mic-action"
            onClick={onStopClick}
            title="Stop and transcribe"
            aria-label="Stop and transcribe"
          >
            <svg viewBox="0 0 24 24" width="13" height="13" fill="currentColor" aria-hidden="true">
              <rect x="7" y="7" width="10" height="10" rx="2" />
            </svg>
          </button>
        </>
      )}

      {phase === "transcribing" && (
        <>
          <span className="mic-spinner" aria-hidden="true" />
          <span className="mic-text">
            Transcribing<span className="mic-ellipsis" aria-hidden="true" />
            <span className="mic-mode-tag">{MODE_LABEL[mode]}</span>
          </span>
        </>
      )}

      {phase === "done" && (
        <>
          <svg className="mic-check" viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" strokeWidth="2.4" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
            <path d="M4 12l5 5L20 6" />
          </svg>
          <span className="mic-text">Pasted</span>
        </>
      )}

      {phase === "error" && (
        <>
          <svg className="mic-warn" viewBox="0 0 24 24" width="15" height="15" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
            <path d="M12 9v4M12 17h.01" />
            <path d="M10.3 3.9 1.8 18a2 2 0 0 0 1.7 3h17a2 2 0 0 0 1.7-3L13.7 3.9a2 2 0 0 0-3.4 0z" />
          </svg>
          <span className="mic-text mic-error-text">{error}</span>
          <button
            type="button"
            className="mic-action"
            onClick={onDismiss}
            title="Dismiss"
            aria-label="Dismiss"
          >
            <svg viewBox="0 0 24 24" width="12" height="12" fill="none" stroke="currentColor" strokeWidth="2.2" strokeLinecap="round" aria-hidden="true">
              <path d="M6 6l12 12M18 6L6 18" />
            </svg>
          </button>
        </>
      )}
    </div>
  );
}

/** getUserMedia rejection → a human message. */
function micErrorMessage(err: unknown): string {
  const name = (err as { name?: string })?.name ?? "";
  if (name === "NotAllowedError" || name === "SecurityError") {
    return "Microphone blocked. Allow mic access for PerfectPrompt in Windows Settings → Privacy → Microphone, then try again.";
  }
  if (name === "NotFoundError" || name === "OverconstrainedError") {
    return "No microphone found. Plug one in and try again.";
  }
  if (name === "NotReadableError") {
    return "The microphone is in use by another app. Close it and try again.";
  }
  return `Couldn't start the mic: ${errorMessage(err)}`;
}

/** Backend command error / generic error → a display string. */
function errorMessage(err: unknown): string {
  if (typeof err === "string") return err;
  const msg = (err as { message?: string })?.message;
  return msg ? String(msg) : "Something went wrong.";
}
