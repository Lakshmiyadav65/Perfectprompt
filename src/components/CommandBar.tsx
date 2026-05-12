import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import "./CommandBar.css";

/// Floating widget — single capsule with drag handle, status toggle,
/// open-app shortcut, and dismiss. Always-on-top, no taskbar entry.
export function CommandBar() {
  const [enabled, setEnabled] = useState(true);
  const appWindow = getCurrentWebviewWindow();

  useEffect(() => {
    // Shell.css sets `html { background: var(--pf-bg) }` for the main
    // window's dark theme. That global rule also targets THIS webview
    // since all windows share the bundle, painting the rectangular dark
    // area we don't want behind the rounded capsule. Inline-overriding
    // the html element keeps that rectangle from showing through.
    document.documentElement.style.background = "transparent";
    document.body.classList.add("cb-route");
    return () => {
      document.documentElement.style.background = "";
      document.body.classList.remove("cb-route");
    };
  }, []);

  useEffect(() => {
    void refresh();
    // Poll so the bar stays in sync if the user flips the sidebar
    // toggle in the main window.
    const id = window.setInterval(() => void refresh(), 1500);
    return () => window.clearInterval(id);
  }, []);

  async function refresh() {
    try {
      const en = await invoke<boolean>("get_hotkey_enabled");
      setEnabled(en);
    } catch (e) {
      console.error("CommandBar refresh failed", e);
    }
  }

  async function handleToggle() {
    const next = !enabled;
    setEnabled(next);
    try {
      await invoke("set_hotkey_enabled", { enabled: next });
    } catch (e) {
      console.error("toggle failed", e);
      setEnabled(!next);
    }
  }

  async function handleOpen() {
    try {
      await invoke("open_main_window");
    } catch (e) {
      console.error("open main failed", e);
    }
  }

  async function handleHide() {
    await appWindow.hide();
  }

  return (
    <div className="cb-row" data-tauri-drag-region>
      {/* Compact drag affordance — purely visual. The whole capsule is
          the actual drag region (data-tauri-drag-region above). This
          icon is the universally-recognized "drag handle" pattern,
          shrunk and tucked at the left so it reads as informative
          without competing with the controls. */}
      <span className="cb-grip" aria-hidden="true">
        <svg viewBox="0 0 6 14" width="6" height="14">
          <circle cx="1.5" cy="2" r="0.9" fill="currentColor" />
          <circle cx="4.5" cy="2" r="0.9" fill="currentColor" />
          <circle cx="1.5" cy="7" r="0.9" fill="currentColor" />
          <circle cx="4.5" cy="7" r="0.9" fill="currentColor" />
          <circle cx="1.5" cy="12" r="0.9" fill="currentColor" />
          <circle cx="4.5" cy="12" r="0.9" fill="currentColor" />
        </svg>
      </span>
      <button
        type="button"
        role="switch"
        aria-checked={enabled}
        className={`cb-toggle ${enabled ? "on" : "off"}`}
        aria-label={enabled ? "Pause PromptForge" : "Activate PromptForge"}
        onClick={() => void handleToggle()}
        title={enabled ? "Active — click to pause" : "Paused — click to activate"}
      >
        <span className="cb-toggle-dot" />
      </button>
      <button
        type="button"
        className="cb-icon-btn"
        aria-label="Open PromptForge"
        onClick={() => void handleOpen()}
        title="Open PromptForge"
      >
        <svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
          <rect x="4" y="6" width="16" height="14" rx="2" />
          <path d="M8 10h8M8 14h5" />
        </svg>
      </button>
      <button
        type="button"
        className="cb-icon-btn"
        aria-label="Hide command bar"
        onClick={() => void handleHide()}
        title="Hide (re-open from tray)"
      >
        <svg viewBox="0 0 24 24" width="12" height="12" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" aria-hidden="true">
          <path d="M6 6l12 12M18 6L6 18" />
        </svg>
      </button>
    </div>
  );
}
