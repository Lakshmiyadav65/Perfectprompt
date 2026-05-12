import { ReactNode, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Home } from "./Home";
import { Settings } from "./Settings";
import { ProjectManager } from "./ProjectManager";
import "./Shell.css";

type Route = "home" | "projects" | "settings";

const NAV: { route: Route; label: string; icon: ReactNode }[] = [
  {
    route: "home",
    label: "Home",
    icon: (
      <svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round">
        <path d="M3 11l9-8 9 8" />
        <path d="M5 10v10h14V10" />
      </svg>
    ),
  },
  {
    route: "projects",
    label: "Projects",
    icon: (
      <svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round">
        <path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v9a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V7z" />
      </svg>
    ),
  },
  {
    route: "settings",
    label: "Settings",
    icon: (
      <svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round">
        <circle cx="12" cy="12" r="3" />
        <path d="M19.4 15a1.7 1.7 0 0 0 .3 1.8l.1.1a2 2 0 1 1-2.8 2.8l-.1-.1a1.7 1.7 0 0 0-1.8-.3 1.7 1.7 0 0 0-1 1.5V21a2 2 0 1 1-4 0v-.1a1.7 1.7 0 0 0-1.1-1.5 1.7 1.7 0 0 0-1.8.3l-.1.1a2 2 0 1 1-2.8-2.8l.1-.1a1.7 1.7 0 0 0 .3-1.8 1.7 1.7 0 0 0-1.5-1H3a2 2 0 1 1 0-4h.1a1.7 1.7 0 0 0 1.5-1 1.7 1.7 0 0 0-.3-1.8l-.1-.1a2 2 0 1 1 2.8-2.8l.1.1a1.7 1.7 0 0 0 1.8.3h.1a1.7 1.7 0 0 0 1-1.5V3a2 2 0 1 1 4 0v.1a1.7 1.7 0 0 0 1 1.5 1.7 1.7 0 0 0 1.8-.3l.1-.1a2 2 0 1 1 2.8 2.8l-.1.1a1.7 1.7 0 0 0-.3 1.8v.1a1.7 1.7 0 0 0 1.5 1H21a2 2 0 1 1 0 4h-.1a1.7 1.7 0 0 0-1.5 1z" />
      </svg>
    ),
  },
];

export function Shell({ initial }: { initial: Route }) {
  const [route, setRoute] = useState<Route>(initial);
  const [hotkey, setHotkey] = useState<string>("Alt+E");

  useEffect(() => {
    invoke<string>("get_hotkey").then(setHotkey).catch(() => {});
  }, []);

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
      // history.replaceState avoids polluting back/forward and prevents an
      // event loop with the hashchange listener above.
      history.replaceState(null, "", desired);
    }
  }, [route]);

  return (
    <div className="pf-shell">
      <aside className="pf-sidebar">
        <div className="pf-brand">
          <div className="pf-brand-mark">PF</div>
          <div className="pf-brand-text">
            <div className="pf-brand-name">PromptForge</div>
            <div className="pf-brand-tag">Basic</div>
          </div>
        </div>

        <nav className="pf-nav">
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
        </nav>

        <div className="pf-sidebar-footer">
          <div className="pf-hotkey-card">
            <div className="pf-hotkey-label">Global hotkey</div>
            <div className="pf-hotkey-value">
              {hotkey.split("+").map((part, i, arr) => (
                <span key={i}>
                  <kbd>{part}</kbd>
                  {i < arr.length - 1 && <span className="pf-plus">+</span>}
                </span>
              ))}
            </div>
            <div className="pf-hotkey-hint">
              Select text anywhere → press to enhance
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
