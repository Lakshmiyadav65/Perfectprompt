import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import { LogicalSize } from "@tauri-apps/api/dpi";
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

/// Window sizes — the capsule alone fits in 160x60, but when the project
/// picker is open we grow the window so a roomy light-themed popover
/// can render inside our own webview. The wider layout matches the
/// macOS-style picker UX with a toggle at the top, a list of options,
/// and a configure footer.
const CAPSULE_SIZE = new LogicalSize(160, 60);
const PICKER_OPEN_SIZE = new LogicalSize(220, 200);

/// Floating widget — single capsule with a project selector (sets the
/// active project that the developer-mode enhancer pulls context from),
/// an enhance button, and a dismiss button. Always-on-top, no taskbar
/// entry.
export function CommandBar() {
  const appWindow = getCurrentWebviewWindow();
  const [store, setStore] = useState<ProjectStore>({
    active_project_id: null,
    projects: [],
  });
  const [pickerOpen, setPickerOpen] = useState(false);

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
    // Poll so the picker reflects projects added/edited in the main
    // window.
    const id = window.setInterval(() => void refreshProjects(), 1500);
    return () => window.clearInterval(id);
  }, []);

  async function openMainProjects() {
    try {
      await invoke("open_main_window");
    } catch (e) {
      console.error("open main window failed", e);
    }
    await closePicker();
  }

  // Close the picker on click outside, Escape, or window blur. Each
  // condition reads the latest pickerOpen via the closure on this
  // effect's run.
  useEffect(() => {
    if (!pickerOpen) return;
    function onMouseDown(e: MouseEvent) {
      const t = e.target as Node;
      if (
        document.querySelector(".cb-picker-pop")?.contains(t) ||
        document.querySelector(".cb-project-wrap")?.contains(t)
      ) {
        return;
      }
      void closePicker();
    }
    function onKey(e: KeyboardEvent) {
      if (e.key === "Escape") void closePicker();
    }
    function onBlur() {
      void closePicker();
    }
    document.addEventListener("mousedown", onMouseDown);
    document.addEventListener("keydown", onKey);
    window.addEventListener("blur", onBlur);
    return () => {
      document.removeEventListener("mousedown", onMouseDown);
      document.removeEventListener("keydown", onKey);
      window.removeEventListener("blur", onBlur);
    };
  }, [pickerOpen]);

  async function refreshProjects() {
    try {
      const data = await invoke<ProjectStore>("list_projects");
      setStore(data);
    } catch (e) {
      console.error("project list refresh failed", e);
    }
  }

  async function openPicker() {
    setPickerOpen(true);
    document.body.classList.add("cb-picker-open");
    try {
      await appWindow.setSize(PICKER_OPEN_SIZE);
    } catch (e) {
      console.error("picker resize failed", e);
    }
  }

  async function closePicker() {
    setPickerOpen(false);
    document.body.classList.remove("cb-picker-open");
    try {
      await appWindow.setSize(CAPSULE_SIZE);
    } catch (e) {
      console.error("picker resize-back failed", e);
    }
  }

  async function pickProject(id: string | null) {
    // Optimistic update so the popover snaps immediately.
    setStore((s) => ({ ...s, active_project_id: id }));
    try {
      if (id) {
        await invoke("set_active_project", { id });
      } else {
        await invoke("clear_active_project");
      }
    } catch (err) {
      console.error("project selection failed", err);
      void refreshProjects();
    }
    await closePicker();
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

  const activeProject = store.projects.find((p) => p.id === store.active_project_id);

  return (
    <div className="cb-shell">
      <div
        className="cb-row"
        role="toolbar"
        aria-label="PerfectPrompt command bar"
        tabIndex={0}
      >
        <button
          type="button"
          className={`cb-icon-btn cb-project-wrap ${store.active_project_id ? "active" : ""}`}
          onClick={() => void (pickerOpen ? closePicker() : openPicker())}
          aria-label="Active project"
          aria-expanded={pickerOpen}
          title={
            activeProject
              ? `Active project: ${activeProject.name}`
              : "Pick a project (optional)"
          }
        >
          <svg
            viewBox="0 0 24 24"
            width="14"
            height="14"
            fill="none"
            stroke="currentColor"
            strokeWidth="1.8"
            strokeLinecap="round"
            strokeLinejoin="round"
            aria-hidden="true"
          >
            <path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v9a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V7z" />
          </svg>
          {store.active_project_id && <span className="cb-project-dot" aria-hidden="true" />}
        </button>
        <button
          type="button"
          className="cb-icon-btn cb-enhance-btn"
          aria-label="Enhance selected text"
          onClick={() => void handleEnhance()}
          title="Enhance the selected text (same as Ctrl+Alt+E)"
        >
          <img
            src="/perfectprompt.svg"
            width="14"
            height="14"
            alt=""
            aria-hidden="true"
            draggable={false}
            className="cb-enhance-logo"
          />
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

      {pickerOpen && (
        <div className="cb-picker-pop" role="dialog" aria-label="PerfectPrompt picker">
          <div className="cb-picker-section" role="listbox" aria-label="Active project">
            <button
              type="button"
              className={`cb-picker-item ${!store.active_project_id ? "active" : ""}`}
              onClick={() => void pickProject(null)}
              role="option"
              aria-selected={!store.active_project_id}
            >
              <span className="cb-picker-item-text">
                <span className="cb-picker-item-name">No project</span>
                <span className="cb-picker-item-hint">enhance without context</span>
              </span>
              {!store.active_project_id && <PickerCheck />}
            </button>
            {store.projects.length === 0 && (
              <div className="cb-picker-empty">
                No projects yet. Open the main app to add one.
              </div>
            )}
            {store.projects.map((p) => (
              <button
                key={p.id}
                type="button"
                className={`cb-picker-item ${p.id === store.active_project_id ? "active" : ""}`}
                onClick={() => void pickProject(p.id)}
                role="option"
                aria-selected={p.id === store.active_project_id}
              >
                <span className="cb-picker-item-text">
                  <span className="cb-picker-item-name">{p.name}</span>
                </span>
                {p.id === store.active_project_id && <PickerCheck />}
              </button>
            ))}
          </div>

          <div className="cb-picker-divider" />

          {/* Escape hatch — open the main app for the full Projects
              management surface (add/edit/delete/links/path). */}
          <button
            type="button"
            className="cb-picker-footer"
            onClick={() => void openMainProjects()}
          >
            <svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
              <circle cx="12" cy="12" r="3" />
              <path d="M19.4 15a1.7 1.7 0 0 0 .3 1.8l.1.1a2 2 0 1 1-2.8 2.8l-.1-.1a1.7 1.7 0 0 0-1.8-.3 1.7 1.7 0 0 0-1 1.5V21a2 2 0 1 1-4 0v-.1a1.7 1.7 0 0 0-1.1-1.5 1.7 1.7 0 0 0-1.8.3l-.1.1a2 2 0 1 1-2.8-2.8l.1-.1a1.7 1.7 0 0 0 .3-1.8 1.7 1.7 0 0 0-1.5-1H3a2 2 0 1 1 0-4h.1a1.7 1.7 0 0 0 1.5-1 1.7 1.7 0 0 0-.3-1.8l-.1-.1a2 2 0 1 1 2.8-2.8l.1.1a1.7 1.7 0 0 0 1.8.3h.1a1.7 1.7 0 0 0 1-1.5V3a2 2 0 1 1 4 0v.1a1.7 1.7 0 0 0 1 1.5 1.7 1.7 0 0 0 1.8-.3l.1-.1a2 2 0 1 1 2.8 2.8l-.1.1a1.7 1.7 0 0 0-.3 1.8v.1a1.7 1.7 0 0 0 1.5 1H21a2 2 0 1 1 0 4h-.1a1.7 1.7 0 0 0-1.5 1z" />
            </svg>
            <span>Configure projects</span>
          </button>
        </div>
      )}
    </div>
  );
}

function PickerCheck() {
  return (
    <svg
      className="cb-picker-item-check"
      viewBox="0 0 16 16"
      width="13"
      height="13"
      fill="none"
      stroke="currentColor"
      strokeWidth="2"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
    >
      <path d="M3 8l3 3 7-7" />
    </svg>
  );
}
