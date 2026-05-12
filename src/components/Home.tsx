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

  useEffect(() => {
    void refresh();
  }, []);

  async function refresh() {
    try {
      const [projects, hk, status] = await Promise.all([
        invoke<ProjectStore>("list_projects"),
        invoke<string>("get_hotkey"),
        invoke<ApiKeyStatus>("api_key_status"),
      ]);
      setStore(projects);
      setHotkey(hk);
      setKeyStatus(status);
    } catch (e) {
      console.error("Home refresh failed", e);
    }
  }

  const active = store.projects.find((p) => p.id === store.active_project_id);
  const totalProjects = store.projects.length;
  const hasKey = keyStatus?.from_env || keyStatus?.from_settings;

  return (
    <div className="ph-page">
      <header className="ph-header">
        <div>
          <h1 className="ph-title">Welcome back</h1>
          <p className="ph-subtitle">
            Your context-aware prompt enhancer is ready. Select text in any
            app, press your hotkey, and get a polished prompt back.
          </p>
        </div>
      </header>

      <section className="ph-hero">
        <div className="ph-hero-text">
          <h2>Make every prompt sound like <em>you</em></h2>
          <p>
            PromptForge skips the questionnaire when you're in your IDE and
            uses your project context. In writing apps it asks a few quick
            questions to nail the tone, audience, and goal.
          </p>
          <button className="ph-cta" onClick={() => onNavigate("projects")}>
            Manage projects →
          </button>
        </div>
        <div className="ph-hero-art" aria-hidden>
          <div className="ph-glow" />
          <div className="ph-hero-glyph">
            <span>⌘</span>
            <strong>{hotkey.split("+").pop()}</strong>
          </div>
        </div>
      </section>

      <section className="ph-stats">
        <div className="ph-stat">
          <div className="ph-stat-num">{totalProjects}</div>
          <div className="ph-stat-label">project{totalProjects === 1 ? "" : "s"}</div>
        </div>
        <div className="ph-stat">
          <div className="ph-stat-num">{hotkey.split("+").length}</div>
          <div className="ph-stat-label">key combo</div>
        </div>
        <div className="ph-stat">
          <div className={`ph-stat-num ${hasKey ? "ok" : "warn"}`}>
            {hasKey ? "Ready" : "Setup"}
          </div>
          <div className="ph-stat-label">
            {hasKey ? "API key configured" : "Add a Groq API key"}
          </div>
        </div>
      </section>

      <section className="ph-section">
        <div className="ph-section-head">
          <h3>Active project</h3>
          {active && (
            <button className="ph-link-btn" onClick={() => onNavigate("projects")}>
              Change →
            </button>
          )}
        </div>

        {active ? (
          <div className="ph-active-card">
            <div className="ph-active-dot" />
            <div className="ph-active-body">
              <div className="ph-active-name">{active.name}</div>
              <div className="ph-active-desc">
                {active.description
                  ? active.description.split("\n")[0].slice(0, 180) +
                    (active.description.length > 180 ? "…" : "")
                  : "No description yet."}
              </div>
              {active.links.length > 0 && (
                <div className="ph-active-links">
                  {active.links.length} link
                  {active.links.length === 1 ? "" : "s"} attached
                </div>
              )}
            </div>
          </div>
        ) : (
          <div className="ph-empty-card">
            <div className="ph-empty-emoji">📁</div>
            <div>
              <strong>No active project</strong>
              <p>
                Add a project so PromptForge can use it as context when you're
                coding.
              </p>
            </div>
            <button
              className="ph-link-btn ph-link-btn--strong"
              onClick={() => onNavigate("projects")}
            >
              Add project →
            </button>
          </div>
        )}
      </section>

      <section className="ph-section">
        <div className="ph-section-head">
          <h3>How it works</h3>
        </div>
        <div className="ph-steps">
          <Step n={1} title="Select text">
            Highlight a rough prompt in any app — IDE, browser, Notepad,
            anywhere.
          </Step>
          <Step n={2} title={`Press ${hotkey}`}>
            PromptForge captures the selection and detects which app you're in.
          </Step>
          <Step n={3} title="Get an enhanced prompt">
            In dev tools you get an instant rewrite. Elsewhere a small popup
            asks a few quick questions first.
          </Step>
        </div>
      </section>

      {!hasKey && (
        <section className="ph-section ph-banner">
          <div>
            <strong>One last thing</strong>
            <p>You need a Groq API key before the first enhancement.</p>
          </div>
          <button
            className="ph-cta ph-cta--small"
            onClick={() => onNavigate("settings")}
          >
            Open Settings
          </button>
        </section>
      )}
    </div>
  );
}

function Step({ n, title, children }: { n: number; title: string; children: React.ReactNode }) {
  return (
    <div className="ph-step">
      <div className="ph-step-n">{n}</div>
      <div>
        <div className="ph-step-title">{title}</div>
        <div className="ph-step-body">{children}</div>
      </div>
    </div>
  );
}
