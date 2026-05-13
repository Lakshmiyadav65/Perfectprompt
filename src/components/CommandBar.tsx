import { useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import "./CommandBar.css";

/// Floating widget — single capsule with drag handle, open-app shortcut,
/// and dismiss. Always-on-top, no taskbar entry.
export function CommandBar() {
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
