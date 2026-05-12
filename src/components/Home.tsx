import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import "./Home.css";

interface Project {
  id: string;
  name: string;
  description: string;
  links: string[];
  created_at: string;
  updated_at: string;
}

interface ProjectStore {
  active_project_id: string | null;
  projects: Project[];
}

interface ApiKeyStatus {
  from_env: boolean;
  from_settings: boolean;
}

export function Home({ onNavigate }: { onNavigate: (r: "projects" | "settings") => void }) {
  const [store, setStore] = useState<ProjectStore>({ active_project_id: null, projects: [] });
  const [hotkey, setHotkey] = useState("Alt+E");
  const [keyStatus, setKeyStatus] = useState<ApiKeyStatus | null>(null);
  const [enabled, setEnabled] = useState(true);

  useEffect(() => {
    void refresh();
    // Poll for changes from the sidebar toggle so the hero status stays
    // in sync without a full page navigation.
    const id = window.setInterval(() => void refresh(), 2000);
    return () => window.clearInterval(id);
  }, []);

  async function refresh() {
    try {
      const [projects, hk, status, en] = await Promise.all([
        invoke<ProjectStore>("list_projects"),
        invoke<string>("get_hotkey"),
        invoke<ApiKeyStatus>("api_key_status"),
        invoke<boolean>("get_hotkey_enabled"),
      ]);
      setStore(projects);
      setHotkey(hk);
      setKeyStatus(status);
      setEnabled(en);
    } catch (e) {
      console.error("Home refresh failed", e);
    }
  }

  const active = store.projects.find((p) => p.id === store.active_project_id);
  const totalProjects = store.projects.length;
  const hasKey = !!(keyStatus?.from_env || keyStatus?.from_settings);

  return (
    <div className="ph-page">
      {/* ---- Header ---- */}
      <header className="ph-header">
        <div className="ph-eyebrow">Welcome back</div>
        <h1 className="ph-title">Refine prompts in place.</h1>
        <p className="ph-subtitle">
          Select rough text in any app, press your hotkey, and PromptForge
          rewrites it into a precise prompt — using your project context
          when you&apos;re coding, asking a few questions when you&apos;re not.
        </p>
      </header>

      {/* ---- Hero: terminal-inspired card with the hotkey as the focal element ---- */}
      <section className="ph-hero">
        <div className="ph-hero-grid" aria-hidden />
        <div className="ph-hero-content">
          <div className="ph-hero-label">Global hotkey</div>
          <div className="ph-hero-keys">
            {hotkey.split("+").map((part, i, arr) => (
              <span key={i} className="ph-hero-keypair">
                <kbd className="ph-hero-kbd">{part}</kbd>
                {i < arr.length - 1 && <span className="ph-hero-plus">+</span>}
              </span>
            ))}
          </div>
          <div className={`ph-hero-meta ${enabled ? "" : "paused"}`}>
            <span className="ph-hero-meta-dot" />
            <span>
              {enabled ? "Listening system-wide" : "Paused — toggle on to resume"}
            </span>
          </div>
        </div>
        <div className="ph-hero-aside">
          <pre className="ph-hero-code">
{`> select text
> press ${hotkey.toLowerCase()}
> ▌`}
          </pre>
        </div>
      </section>

      {/* ---- Status grid: 3 dense info tiles ---- */}
      <section className="ph-tiles">
        <Tile
          label="Projects"
          value={String(totalProjects)}
          caption={totalProjects === 0 ? "none added yet" : "stored locally"}
          actionLabel={totalProjects === 0 ? "Add" : "Manage"}
          onAction={() => onNavigate("projects")}
        />
        <Tile
          label="API key"
          value={hasKey ? "Connected" : "Missing"}
          caption={
            keyStatus?.from_env
              ? "from .env"
              : keyStatus?.from_settings
                ? "from settings"
                : "not configured"
          }
          accent={!hasKey}
          actionLabel="Settings"
          onAction={() => onNavigate("settings")}
        />
        <Tile
          label="Active project"
          value={active?.name ?? "—"}
          caption={active ? "in use as context" : "no context attached"}
          actionLabel={active ? "Change" : "Add"}
          onAction={() => onNavigate("projects")}
        />
      </section>

      {/* ---- Active project deep card ---- */}
      <section className="ph-section">
        <div className="ph-section-head">
          <h3>Active project</h3>
          {active && (
            <button className="ph-link-btn" onClick={() => onNavigate("projects")}>
              Open Projects ↗
            </button>
          )}
        </div>

        {active ? (
          <div className="ph-active-card">
            <div className="ph-active-name">
              {active.name}
              <span className="ph-active-flag">active</span>
            </div>
            <div className="ph-active-desc">
              {active.description
                ? active.description.split("\n")[0].slice(0, 220) +
                  (active.description.length > 220 ? "…" : "")
                : "No description yet — add one so prompts have something to chew on."}
            </div>
            {active.links.length > 0 && (
              <div className="ph-active-links">
                {active.links.length} link
                {active.links.length === 1 ? "" : "s"} attached
              </div>
            )}
          </div>
        ) : (
          <div className="ph-empty-card">
            <div className="ph-empty-text">
              <strong>No active project.</strong>
              <span>
                Add one to give PromptForge codebase awareness when you&apos;re in
                an IDE. The questionnaire still works without it.
              </span>
            </div>
            <button
              className="ph-cta"
              onClick={() => onNavigate("projects")}
            >
              Add project
            </button>
          </div>
        )}
      </section>

      {/* ---- Keyboard reference ---- */}
      <section className="ph-section">
        <div className="ph-section-head">
          <h3>Keyboard</h3>
        </div>
        <div className="ph-kbd-rows">
          <KbdRow keys={hotkey.split("+")}>Capture and enhance the selected text.</KbdRow>
          <KbdRow keys={["Shift", ...hotkey.split("+")]}>
            Bypass the questionnaire even in non-developer apps.
          </KbdRow>
          <KbdRow keys={["Esc"]}>
            Dismiss the question card without enhancing.
          </KbdRow>
        </div>
      </section>

      {!hasKey && (
        <section className="ph-banner">
          <div>
            <strong>Add a Groq API key to start.</strong>
            <p>Free at console.groq.com — takes about 30 seconds.</p>
          </div>
          <button className="ph-cta" onClick={() => onNavigate("settings")}>
            Open Settings
          </button>
        </section>
      )}
    </div>
  );
}

function Tile({
  label,
  value,
  caption,
  actionLabel,
  onAction,
  accent,
}: {
  label: string;
  value: string;
  caption: string;
  actionLabel: string;
  onAction: () => void;
  accent?: boolean;
}) {
  return (
    <div className={`ph-tile ${accent ? "accent" : ""}`}>
      <div className="ph-tile-label">{label}</div>
      <div className="ph-tile-value">{value}</div>
      <div className="ph-tile-row">
        <span className="ph-tile-caption">{caption}</span>
        <button className="ph-tile-action" onClick={onAction}>
          {actionLabel} →
        </button>
      </div>
    </div>
  );
}

function KbdRow({ keys, children }: { keys: string[]; children: React.ReactNode }) {
  return (
    <div className="ph-kbd-row">
      <div className="ph-kbd-row-keys">
        {keys.map((k, i, arr) => (
          <span key={i} className="ph-kbd-keypair">
            <kbd className="ph-kbd-mini">{k}</kbd>
            {i < arr.length - 1 && <span className="ph-kbd-plus">+</span>}
          </span>
        ))}
      </div>
      <div className="ph-kbd-row-text">{children}</div>
    </div>
  );
}
