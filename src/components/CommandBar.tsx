import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import "./CommandBar.css";

/// Minimal floating widget — just the active/paused toggle. Always-on-top.
/// The pill background is draggable; the toggle inside fires its own click.
export function CommandBar() {
  const [enabled, setEnabled] = useState(true);

  useEffect(() => {
    document.body.classList.add("cb-route");
    return () => {
      document.body.classList.remove("cb-route");
    };
  }, []);

  useEffect(() => {
    void refresh();
    // Poll so the bar stays in sync if the user flips the sidebar toggle
    // in the main window.
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

  return (
    <div className="cb-root" data-tauri-drag-region>
      <button
        type="button"
        role="switch"
        aria-checked={enabled}
        aria-label={enabled ? "Pause PromptForge" : "Activate PromptForge"}
        title={enabled ? "Active — click to pause" : "Paused — click to activate"}
        className={`cb-toggle ${enabled ? "on" : "off"}`}
        onClick={() => void handleToggle()}
      >
        <span className="cb-toggle-dot" />
      </button>
    </div>
  );
}
