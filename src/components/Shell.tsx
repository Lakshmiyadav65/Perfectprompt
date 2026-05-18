import { ReactNode, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Home } from "./Home";
import { Settings } from "./Settings";
import { ProjectManager } from "./ProjectManager";
import { useEnhancementUsage } from "../hooks/useEnhancementUsage";
import { useAuth } from "../hooks/useAuth";
import { useDisplayName } from "../hooks/useDisplayName";
import "./Shell.css";

type Route = "home" | "projects" | "settings";
export type FocusTarget = "api-key" | null;

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

// Reads (and clears) a pending post-auth focus breadcrumb dropped by
// PostAuthSetup. Returns the initial route + focus target Shell should
// boot into. Falls back to whatever the parent passed (typically
// "home") when nothing is pending.
function consumePostAuthBreadcrumb(fallback: Route): {
  route: Route;
  focus: FocusTarget;
} {
  try {
    const pending = sessionStorage.getItem("pf:post-auth-focus");
    if (pending === "api-key") {
      sessionStorage.removeItem("pf:post-auth-focus");
      return { route: "settings", focus: "api-key" };
    }
  } catch {
    // sessionStorage can throw in private-mode embeds / sandboxes —
    // not worth crashing the shell over.
  }
  return { route: fallback, focus: null };
}

export function Shell({ initial }: { initial: Route }) {
  // Boot lazily from the breadcrumb so we never flicker through Home
  // on the way to Settings — the user explicitly asked for the API
  // setup section, hand it to them on the first render. Lazy init so
  // the sessionStorage read + clear runs exactly once.
  const [boot] = useState(() => consumePostAuthBreadcrumb(initial));
  const [route, setRoute] = useState<Route>(boot.route);
  // When a deep-link CTA (Home setup card, banner, status tile) sends
  // the user to Settings, we pass an explicit focus target so Settings
  // can scroll + focus the right section instead of dumping them at
  // the page top.
  const [focusTarget, setFocusTarget] = useState<FocusTarget>(boot.focus);
  const [hotkey, setHotkey] = useState<string>("Ctrl+Alt+E");
  const [keyStatus, setKeyStatus] = useState<ApiKeyStatus | null>(null);
  const [enabled, setEnabled] = useState<boolean>(true);
  const [toggling, setToggling] = useState(false);
  const [store, setStore] = useState<ProjectStore>({ active_project_id: null, projects: [] });
  const usage = useEnhancementUsage();
  const usagePct = Math.min(100, Math.round((usage.used / usage.limit) * 100));
  const auth = useAuth();
  const displayName = useDisplayName(auth.user);

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

  // Cross-screen navigation with an optional focus target. Sidebar
  // and tray callers pass no target (just route changes); CTA
  // surfaces like the Home setup card pass "api-key" so Settings
  // scrolls + focuses the Groq API Key section on arrival.
  function navigate(next: Route, focus: FocusTarget = null) {
    if (focus) setFocusTarget(focus);
    setRoute(next);
  }

  const ready = !!(keyStatus?.from_env || keyStatus?.from_settings);
  // Tauri returns the canonical "CommandOrControl" / "Option" / "Super"
  // strings — too verbose for the 232px sidebar. Display the short,
  // OS-friendly forms.
  const hotkeyParts = hotkey.split("+").map(prettyKey);

  // Mocked "Top 2%" badge gate: shows only when the user is fully set
  // up (API key configured AND at least one project attached). The
  // copy ("Top 2%") is decorative until we ship real telemetry — the
  // intent is to let power users feel the app recognise them. Wire
  // to a real percentile once enhancement history lives on disk.
  const isPowerUser = ready && store.projects.length > 0;
  // Top 2 most-recently-updated projects for the sidebar "Recent" list.
  const recentProjects = [...store.projects]
    .sort((a, b) => (a.updated_at < b.updated_at ? 1 : -1))
    .slice(0, 3);

  return (
    <div className="pf-shell">
      <aside className="pf-sidebar">
        <div className="pf-brand">
          <BrandMark />
          <div className="pf-brand-name">PerfectPrompt</div>
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
              {item.route === "projects" && (
                <span className="pf-beta-badge" aria-label="Beta feature">Beta</span>
              )}
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
          {/* Daily enhancement quota — frontend-only counter persisted
              in localStorage, resets on the local date change. Sits
              above the listening card per design. */}
          <div className={`pf-usage-card ${usage.limitReached ? "limit-reached" : ""}`}>
            <div className="pf-usage-head">
              <span className="pf-usage-label">Enhancements</span>
              <span className="pf-usage-count">
                <strong>{usage.used}</strong>
                <span className="pf-usage-sep">/</span>
                {usage.limit}
              </span>
            </div>
            <div className="pf-usage-bar" aria-hidden="true">
              <div
                className="pf-usage-bar-fill"
                style={{ width: `${usagePct}%` }}
              />
            </div>
            <div className="pf-usage-hint">
              {usage.limitReached
                ? "Daily enhancement limit reached."
                : "Daily limit"}
            </div>
          </div>

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
                title={enabled ? "Pause PerfectPrompt" : "Activate PerfectPrompt"}
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

          {/* Profile chip — Discord/Slack style anchor for user identity.
              Name resolves via useDisplayName: override > Google name >
              email local-part > "You". */}
          <button
            type="button"
            className="pf-profile-chip"
            onClick={() => setRoute("settings")}
            title="Open Settings"
            aria-label={`${displayName.name} — open Settings`}
          >
            <span className="pf-profile-avatar" aria-hidden="true">
              {displayName.name.charAt(0).toUpperCase()}
            </span>
            <span className="pf-profile-text">
              <span className="pf-profile-name-row">
                <span className="pf-profile-name">{displayName.name}</span>
                {isPowerUser && (
                  <span
                    className="pf-profile-badge"
                    title="Top 2% — based on your local activity"
                    aria-label="Top two percent"
                  >
                    <svg
                      viewBox="0 0 24 24"
                      width="9"
                      height="9"
                      fill="currentColor"
                      aria-hidden="true"
                    >
                      <path d="M12 2l2.6 6.3 6.8.6-5.2 4.5 1.6 6.6L12 16.8 6.2 20l1.6-6.6L2.6 8.9l6.8-.6L12 2z" />
                    </svg>
                    Top 2%
                  </span>
                )}
              </span>
            </span>
            <svg
              className="pf-profile-chev"
              viewBox="0 0 24 24"
              width="14"
              height="14"
              fill="none"
              stroke="currentColor"
              strokeWidth="1.6"
              strokeLinecap="round"
              strokeLinejoin="round"
              aria-hidden="true"
            >
              <path d="M9 6l6 6-6 6" />
            </svg>
          </button>
        </div>
      </aside>

      <main className="pf-main">
        {route === "home" && <Home onNavigate={navigate} />}
        {route === "projects" && <ProjectManager />}
        {route === "settings" && (
          <Settings
            focusTarget={focusTarget}
            onFocusHandled={() => setFocusTarget(null)}
          />
        )}
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

/// Translates Tauri's canonical modifier names to the short labels users
/// expect on Windows. macOS users would see ⌘/⌥ — we'll branch when we
/// ship that platform.
function prettyKey(part: string): string {
  switch (part.trim()) {
    case "CommandOrControl":
    case "Control":
      return "Ctrl";
    case "Option":
    case "Alt":
      return "Alt";
    case "Super":
    case "Command":
      return "Win";
    default:
      return part;
  }
}
