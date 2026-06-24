import { useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import "./Toast.css";

/**
 * In-app PerfectPrompt-styled toast. Renders inside the borderless,
 * always-on-top `toast` window (`#/toast` route) — the Rust side in
 * `toast_window.rs` positions and shows the window; this component
 * listens for `pipeline:fallback` events to pick up the message,
 * animates in, auto-dismisses, and asks Tauri to hide the window
 * back when the slide-out finishes.
 *
 * Two visual phases:
 *   - `visible: true`  → slide-in (opacity 0→1, translateY 12→0)
 *   - `visible: false` → slide-out (the reverse) then `window.hide()`
 *
 * Auto-dismiss after 3500ms. The X button skips the wait.
 */
export function Toast() {
  const [message, setMessage] = useState<string>("");
  const [visible, setVisible] = useState<boolean>(false);
  const dismissTimer = useRef<number | null>(null);
  const hideTimer = useRef<number | null>(null);

  // Strip the page background so the borderless window stays truly
  // transparent — same workaround StatusIndicator uses (Shell.css
  // unconditionally paints --pf-bg-deep on html/body/#root).
  useEffect(() => {
    document.body.classList.add("toast-route");
    const prevHtmlBg = document.documentElement.style.background;
    document.documentElement.style.background = "transparent";
    return () => {
      document.body.classList.remove("toast-route");
      document.documentElement.style.background = prevHtmlBg;
    };
  }, []);

  // Subscribe to the pipeline-fallback event. The Rust side fires
  // this every time a fallback happens (rate-limit, validator
  // reject, intake reject, etc.). The same toast window handles all
  // of them — the message text comes from Rust's friendly_reason.
  useEffect(() => {
    const unlistenPromise = listen<string>("pipeline:fallback", (event) => {
      const text = event.payload || "Kept original.";
      setMessage(text);
      // Slight raf-delay so the entry-animation runs from the
      // initial-hidden state on every event, even repeated ones.
      requestAnimationFrame(() => setVisible(true));

      // Cancel any prior dismiss/hide timers so a new toast resets
      // the dismiss clock instead of inheriting the old one.
      if (dismissTimer.current !== null) {
        window.clearTimeout(dismissTimer.current);
      }
      if (hideTimer.current !== null) {
        window.clearTimeout(hideTimer.current);
      }
      dismissTimer.current = window.setTimeout(() => {
        setVisible(false);
        hideTimer.current = window.setTimeout(() => {
          // Ask Tauri to hide the window once the slide-out
          // animation completes. The window stays in the manager
          // (`prevent_close` keep-alive) so the next show is cheap.
          void getCurrentWindow().hide();
        }, 240);
      }, 3500);
    });

    return () => {
      void unlistenPromise.then((unlisten) => unlisten());
      if (dismissTimer.current !== null) {
        window.clearTimeout(dismissTimer.current);
      }
      if (hideTimer.current !== null) {
        window.clearTimeout(hideTimer.current);
      }
    };
  }, []);

  function handleClose() {
    setVisible(false);
    if (dismissTimer.current !== null) {
      window.clearTimeout(dismissTimer.current);
      dismissTimer.current = null;
    }
    if (hideTimer.current !== null) {
      window.clearTimeout(hideTimer.current);
    }
    hideTimer.current = window.setTimeout(() => {
      void getCurrentWindow().hide();
    }, 240);
  }

  return (
    <div className={`pf-toast ${visible ? "pf-toast-visible" : ""}`} role="status" aria-live="polite">
      <span className="pf-toast-dot" aria-hidden />
      <div className="pf-toast-body">
        <span className="pf-toast-message">{message || "Kept original."}</span>
      </div>
      <button
        type="button"
        className="pf-toast-close"
        onClick={handleClose}
        aria-label="Dismiss notification"
      >
        <svg viewBox="0 0 16 16" width="12" height="12" fill="none" stroke="currentColor" strokeWidth="1.75" strokeLinecap="round" strokeLinejoin="round" aria-hidden>
          <path d="M3 3l10 10M13 3L3 13" />
        </svg>
      </button>
    </div>
  );
}
