import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import "./CommandBar.css";

interface Project {
  id: string;
  name: string;
  description: string;
  links: string[];
  path: string | null;
  created_at: string;
  updated_at: string;
}

interface ProjectStore {
  active_project_id: string | null;
  projects: Project[];
}

/// Floating widget — single capsule with a project selector (sets the
/// active project that the developer-mode enhancer pulls context from)
/// and a dismiss button. Always-on-top, no taskbar entry.
export function CommandBar() {
  const appWindow = getCurrentWebviewWindow();
  const [store, setStore] = useState<ProjectStore>({
    active_project_id: null,
    projects: [],
  });

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
    void refreshProjects();
    // Poll so the dropdown picks up projects added/edited in the main
    // window. Same cadence as the sidebar toggle poll.
    const id = window.setInterval(() => void refreshProjects(), 1500);
    return () => window.clearInterval(id);
  }, []);

  async function refreshProjects() {
    try {
      const data = await invoke<ProjectStore>("list_projects");
      setStore(data);
    } catch (e) {
      console.error("project list refresh failed", e);
    }
  }

  async function handleProjectChange(e: React.ChangeEvent<HTMLSelectElement>) {
    const id = e.target.value;
    if (!id) return;
    // Optimistic update so the select snaps immediately.
    setStore((s) => ({ ...s, active_project_id: id }));
    try {
      await invoke("set_active_project", { id });
    } catch (err) {
      console.error("set active project failed", err);
      void refreshProjects();
    }
  }

  async function handleEnhance() {
    try {
      await invoke("trigger_enhance", { bypass: false });
    } catch (e) {
      // Failures (empty selection, API error, etc.) surface as backend
      // log lines; we don't have a visible toast surface in the capsule
      // so console-log is the right level.
      console.error("enhance trigger failed", e);
    }
  }

  async function handleHide() {
    // X is equivalent to flipping the sidebar toggle to OFF: the capsule
    // disappears AND the persisted "enabled" setting flips to false, so
    // re-opening the main app shows the toggle as Paused. Hide the window
    // first for instant feedback, then persist.
    await appWindow.hide();
    try {
      await invoke("set_hotkey_enabled", { enabled: false });
    } catch (e) {
      console.error("persist paused state failed", e);
    }
  }

  const hasProjects = store.projects.length > 0;

  return (
    <div className="cb-row">
      <select
        className="cb-project-select"
        value={store.active_project_id ?? ""}
        onChange={(e) => void handleProjectChange(e)}
        disabled={!hasProjects}
        aria-label="Active project"
        title={hasProjects ? "Active project (scanned for context)" : "Add a project in the main app"}
      >
        {!hasProjects && <option value="">No projects</option>}
        {hasProjects && store.active_project_id === null && (
          <option value="">— pick a project —</option>
        )}
        {store.projects.map((p) => (
          <option key={p.id} value={p.id}>
            {p.name}
          </option>
        ))}
      </select>
      <button
        type="button"
        className="cb-icon-btn cb-enhance-btn"
        aria-label="Enhance selected text"
        onClick={() => void handleEnhance()}
        title="Enhance the selected text (same as Ctrl+Alt+E)"
      >
        {/* Sparkles / wand glyph — signals "improve / transform". */}
        <svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
          <path d="M12 3v3M12 18v3M3 12h3M18 12h3" />
          <path d="M5.6 5.6l2.1 2.1M16.3 16.3l2.1 2.1M5.6 18.4l2.1-2.1M16.3 7.7l2.1-2.1" />
          <circle cx="12" cy="12" r="3" />
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
