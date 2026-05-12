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

const NAV: { route: Route; label: string; icon: ReactNode }[] = [
  {
    route: "home",
    label: "Home",
    icon: (
      <svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round">
        <path d="M3 11l9-8 9 8" />
        <path d="M5 10v10h14V10" />
      </svg>
    ),
  },
  {
    route: "projects",
    label: "Projects",
    icon: (
      <svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round">
        <path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v9a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V7z" />
      </svg>
    ),
  },
  {
    route: "settings",
    label: "Settings",
    icon: (
      <svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round">
        <circle cx="12" cy="12" r="3" />
        <path d="M19.4 15a1.7 1.7 0 0 0 .3 1.8l.1.1a2 2 0 1 1-2.8 2.8l-.1-.1a1.7 1.7 0 0 0-1.8-.3 1.7 1.7 0 0 0-1 1.5V21a2 2 0 1 1-4 0v-.1a1.7 1.7 0 0 0-1.1-1.5 1.7 1.7 0 0 0-1.8.3l-.1.1a2 2 0 1 1-2.8-2.8l.1-.1a1.7 1.7 0 0 0 .3-1.8 1.7 1.7 0 0 0-1.5-1H3a2 2 0 1 1 0-4h.1a1.7 1.7 0 0 0 1.5-1 1.7 1.7 0 0 0-.3-1.8l-.1-.1a2 2 0 1 1 2.8-2.8l.1.1a1.7 1.7 0 0 0 1.8.3h.1a1.7 1.7 0 0 0 1-1.5V3a2 2 0 1 1 4 0v.1a1.7 1.7 0 0 0 1 1.5 1.7 1.7 0 0 0 1.8-.3l.1-.1a2 2 0 1 1 2.8 2.8l-.1.1a1.7 1.7 0 0 0-.3 1.8v.1a1.7 1.7 0 0 0 1.5 1H21a2 2 0 1 1 0 4h-.1a1.7 1.7 0 0 0-1.5 1z" />
      </svg>
    ),
  },
];

/// Custom forge-flame brand mark. Anvil silhouette + flame tip, monoline,
/// renders crisply at 28px. The orange is the only color in the entire UI.
function ForgeMark() {
  return (
    <svg viewBox="0 0 32 32" width="28" height="28" aria-hidden="true">
      {/* Flame */}
      <path
        d="M16 4c-1.2 3.5-3.6 5.5-3.6 8.4 0 2 1.6 3.6 3.6 3.6s3.6-1.6 3.6-3.6c0-1.6-.8-2.5-1.7-3.6.3 1.6-.5 2.6-1.3 2.6-1 0-1.4-1-1-2 .6-1.5 1.4-3 .4-5.4z"
        fill="var(--pf-accent)"
      />
      {/* Anvil */}
      <path
        d="M7 21h18v3H7zM10 24h12v2l-2 2H12l-2-2z"
        fill="var(--pf-text)"
      />
    </svg>
  );
}

export function Shell({ initial }: { initial: Route }) {
  const [route, setRoute] = useState<Route>(initial);
  const [hotkey, setHotkey] = useState<string>("Alt+E");
  const [keyStatus, setKeyStatus] = useState<ApiKeyStatus | null>(null);

  useEffect(() => {
    invoke<string>("get_hotkey").then(setHotkey).catch(() => {});
    invoke<ApiKeyStatus>("api_key_status").then(setKeyStatus).catch(() => {});
  }, [route]);

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

  // Keep the URL hash in sync when the user clicks sidebar nav so the next
  // hashchange from the tray doesn't fight the React state.
  useEffect(() => {
    const desired = `#/${route}`;
    if (window.location.hash !== desired) {
      history.replaceState(null, "", desired);
    }
  }, [route]);

  const ready = !!(keyStatus?.from_env || keyStatus?.from_settings);

  return (
    <div className="pf-shell">
      <aside className="pf-sidebar">
        <div className="pf-brand">
          <ForgeMark />
          <div className="pf-brand-word">PromptForge</div>
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
              {route === item.route && <span className="pf-nav-marker" aria-hidden />}
            </button>
          ))}
        </nav>

        <div className="pf-sidebar-footer">
          <div className={`pf-sb-pill ${ready ? "ready" : "setup"}`}>
            <span className="pf-sb-dot" aria-hidden />
            <span>{ready ? "Ready" : "Setup needed"}</span>
          </div>
          <div className="pf-hotkey-block" aria-label="Global hotkey">
            <div className="pf-hotkey-caption">Press from anywhere</div>
            <div className="pf-hotkey-keys">
              {hotkey.split("+").map((part, i, arr) => (
                <span key={i} className="pf-hotkey-keypair">
                  <kbd className="pf-kbd">{part}</kbd>
                  {i < arr.length - 1 && <span className="pf-hotkey-plus">+</span>}
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
