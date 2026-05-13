import { ReactNode, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Home } from "./Home";
import { Settings } from "./Settings";
import { ProjectManager } from "./ProjectManager";
import "./Shell.css";

type Route = "home" | "projects" | "settings";

interface ApiKeyStatus {
  from_env: boolean;
  from_settings: boolean;
}

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

const NAV: { route: Route; label: string; icon: ReactNode }[] = [
  {
    route: "home",
    label: "Home",
    icon: (
      <svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round">
        <path d="M3 12 12 3l9 9" />
        <path d="M5 10v10h14V10" />
      </svg>
    ),
  },
  {
    route: "projects",
    label: "Projects",
    icon: (
      <svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round">
        <path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v9a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V7z" />
      </svg>
    ),
  },
  {
    route: "settings",
    label: "Settings",
    icon: (
      <svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round">
        <circle cx="12" cy="12" r="3" />
        <path d="M19.4 15a1.7 1.7 0 0 0 .3 1.8l.1.1a2 2 0 1 1-2.8 2.8l-.1-.1a1.7 1.7 0 0 0-1.8-.3 1.7 1.7 0 0 0-1 1.5V21a2 2 0 1 1-4 0v-.1a1.7 1.7 0 0 0-1.1-1.5 1.7 1.7 0 0 0-1.8.3l-.1.1a2 2 0 1 1-2.8-2.8l.1-.1a1.7 1.7 0 0 0 .3-1.8 1.7 1.7 0 0 0-1.5-1H3a2 2 0 1 1 0-4h.1a1.7 1.7 0 0 0 1.5-1 1.7 1.7 0 0 0-.3-1.8l-.1-.1a2 2 0 1 1 2.8-2.8l.1.1a1.7 1.7 0 0 0 1.8.3h.1a1.7 1.7 0 0 0 1-1.5V3a2 2 0 1 1 4 0v.1a1.7 1.7 0 0 0 1 1.5 1.7 1.7 0 0 0 1.8-.3l.1-.1a2 2 0 1 1 2.8 2.8l-.1.1a1.7 1.7 0 0 0-.3 1.8v.1a1.7 1.7 0 0 0 1.5 1H21a2 2 0 1 1 0 4h-.1a1.7 1.7 0 0 0-1.5 1z" />
      </svg>
    ),
  },
];

export function Shell({ initial }: { initial: Route }) {
  const [route, setRoute] = useState<Route>(initial);
  const [hotkey, setHotkey] = useState<string>("Ctrl+Alt+E");
  const [keyStatus, setKeyStatus] = useState<ApiKeyStatus | null>(null);
  const [enabled, setEnabled] = useState<boolean>(true);
  const [toggling, setToggling] = useState(false);
  const [store, setStore] = useState<ProjectStore>({ active_project_id: null, projects: [] });

  useEffect(() => {
    invoke<string>("get_hotkey").then(setHotkey).catch(() => {});
    invoke<ApiKeyStatus>("api_key_status").then(setKeyStatus).catch(() => {});
    invoke<boolean>("get_hotkey_enabled").then(setEnabled).catch(() => {});
    invoke<ProjectStore>("list_projects").then(setStore).catch(() => {});
  }, [route]);

  // Poll for external state flips (e.g. capsule X persists Paused).
  useEffect(() => {
    const id = window.setInterval(() => {
      if (toggling) return;
      invoke<boolean>("get_hotkey_enabled").then(setEnabled).catch(() => {});
      invoke<ProjectStore>("list_projects").then(setStore).catch(() => {});
    }, 1500);
    return () => window.clearInterval(id);
  }, [toggling]);

  async function handleToggle() {
    if (toggling) return;
    setToggling(true);
    const prev = enabled;
    const next = !enabled;
    setEnabled(next);
    try {
      await invoke("set_hotkey_enabled", { enabled: next });
      try {
        await invoke(next ? "show_command_bar" : "hide_command_bar");
      } catch (e) {
        console.error("command bar visibility toggle failed", e);
      }
    } catch (e) {
      console.error("toggle failed", e);
      setEnabled(prev);
    } finally {
      setToggling(false);
    }
  }

  // Tray menu items navigate by setting `window.location.hash`. Listen for
  // those changes so the sidebar stays in sync without a full reload.
  useEffect(() => {
    function handler() {
      const hash = window.location.hash.replace(/^#\//, "").trim();
      if (hash === "projects" || hash === "settings" || hash === "home") {
        setRoute(hash);
      }
    }
    window.addEventListener("hashchange", handler);
    return () => window.removeEventListener("hashchange", handler);
  }, []);

  useEffect(() => {
    const desired = `#/${route}`;
    if (window.location.hash !== desired) {
      history.replaceState(null, "", desired);
    }
  }, [route]);

  const ready = !!(keyStatus?.from_env || keyStatus?.from_settings);
  const hotkeyParts = hotkey.split("+");
  // Top 2 most-recently-updated projects for the sidebar "Recent" list.
  const recentProjects = [...store.projects]
    .sort((a, b) => (a.updated_at < b.updated_at ? 1 : -1))
    .slice(0, 3);

  return (
    <div className="pf-shell">
      <aside className="pf-sidebar">
        <div className="pf-brand">
          <BrandMark />
          <div className="pf-brand-name">PromptForge</div>
        </div>

        <nav className="pf-nav" aria-label="Primary">
          {NAV.map((item) => (
            <button
              key={item.route}
              type="button"
              className={`pf-nav-item ${route === item.route ? "active" : ""}`}
              onClick={() => setRoute(item.route)}
            >
              <span className="pf-nav-icon">{item.icon}</span>
              <span>{item.label}</span>
            </button>
          ))}

          {recentProjects.length > 0 && (
            <>
              <div className="pf-nav-section">Recent</div>
              {recentProjects.map((p) => (
                <button
                  key={p.id}
                  type="button"
                  className="pf-nav-item pf-nav-recent"
                  onClick={() => setRoute("projects")}
                >
                  <span
                    className={`pf-recent-dot ${
                      p.id === store.active_project_id ? "active" : ""
                    }`}
                  />
                  <span className="pf-recent-name">{p.name}</span>
                </button>
              ))}
            </>
          )}
        </nav>

        <div className="pf-sidebar-bottom">
          <div className={`pf-listening-card ${enabled && ready ? "live" : ""}`}>
            <div className="pf-listening-row">
              <span className="pf-listening-dot" />
              <span className="pf-listening-label">
                {ready ? (enabled ? "Listening" : "Paused") : "Setup"}
              </span>
              <button
                type="button"
                className={`pf-toggle-mini ${enabled ? "on" : ""}`}
                onClick={() => void handleToggle()}
                disabled={toggling || !ready}
                aria-label={enabled ? "Pause" : "Activate"}
                title={enabled ? "Pause PromptForge" : "Activate PromptForge"}
              >
                <span className="pf-toggle-mini-dot" />
              </button>
            </div>
            <div className="pf-listening-hint">Trigger from anywhere</div>
            <div className="pf-listening-keys">
              {hotkeyParts.map((part, i) => (
                <span key={i} className="pf-keypair">
                  <kbd className="pf-kbd">{part}</kbd>
                  {i < hotkeyParts.length - 1 && (
                    <span className="pf-kbd-plus">+</span>
                  )}
                </span>
              ))}
            </div>
          </div>
        </div>
      </aside>

      <main className="pf-main">
        {route === "home" && <Home onNavigate={setRoute} />}
        {route === "projects" && <ProjectManager />}
        {route === "settings" && <Settings />}
      </main>
    </div>
  );
}

/// Conic-gradient brand mark — matches the mockup. No fill image needed,
/// renders crisply at 28px.
function BrandMark() {
  return (
    <div className="pf-brand-mark" aria-hidden="true" />
  );
}
