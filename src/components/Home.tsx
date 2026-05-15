import { useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import "./Home.css";

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

interface ApiKeyStatus {
  from_env: boolean;
  from_settings: boolean;
}

/// Hard-coded sample rows for the "Recent enhancements" list. PromptForge
/// doesn't persist enhancement history yet — these are illustrative.
/// TODO: wire to a real Rust-side log once we have on-disk storage.
const SAMPLE_RECENT = [
  {
    time: "2:08 PM",
    from: "fix the dashboard",
    to: "Refactor the Analytics dashboard's initial-load path to reduce TTI…",
  },
  {
    time: "1:42 PM",
    from: "email my manager about PTO",
    to: "Write a short, warm-but-professional email to my manager…",
  },
  {
    time: "12:30 PM",
    from: "linkedin post about shipping V1",
    to: "Write a LinkedIn post announcing PromptForge V1 with a behind-the-scenes vibe…",
  },
  {
    time: "11:18 AM",
    from: "make this image cinematic",
    to: "Transform the attached image into a cinematic, moody portrait…",
  },
];

/// Same caveat as SAMPLE_RECENT — 7-day activity bars are mocked
/// against a believable pattern until we have telemetry.
const SAMPLE_ACTIVITY: Array<"" | "lo" | "md" | "hi"> = ["lo", "md", "lo", "hi", "md", "", "md"];

function dayTimeNow(): string {
  const d = new Date();
  const weekday = d.toLocaleDateString(undefined, { weekday: "long" });
  const time = d.toLocaleTimeString(undefined, {
    hour: "numeric",
    minute: "2-digit",
  });
  return `${weekday}, ${time}`;
}

export function Home({ onNavigate }: { onNavigate: (r: "projects" | "settings") => void }) {
  const [store, setStore] = useState<ProjectStore>({ active_project_id: null, projects: [] });
  const [hotkey, setHotkey] = useState("Ctrl+Alt+E");
  const [keyStatus, setKeyStatus] = useState<ApiKeyStatus | null>(null);
  const [enabled, setEnabled] = useState(true);
  // Key on the demo pane — bumping this forces a remount so the CSS
  // animations restart from t=0.
  const [demoKey, setDemoKey] = useState(0);

  useEffect(() => {
    void refresh();
    const id = window.setInterval(() => void refresh(), 2000);
    return () => window.clearInterval(id);
  }, []);

  // Demo loop = 18.5s. Output paragraphs in pane 3 stagger in at
  // 7.9 / 8.55 / 9.2 / 9.85s (0.65s gaps, 0.3s fades — 2× the prior
  // pace). Last paragraph settles at ~10.15s, leaving a ~8s reading
  // hold before the React `key` change snaps the stage back to t=0.
  // Panes 1 and 2 are unaffected.
  useEffect(() => {
    const id = window.setInterval(() => setDemoKey((k) => k + 1), 18500);
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
  const hasKey = !!(keyStatus?.from_env || keyStatus?.from_settings);
  const dayTime = useMemo(dayTimeNow, []);
  // Tauri's canonical "CommandOrControl" reads too long inside the
  // small strip card and demo chip — use the friendly Windows form.
  const hotkeyParts = hotkey.split("+").map(prettyKey);

  return (
    <div className="ph-page">

      <div className="ph-main-inner">

        {/* ===== HERO ===== */}
        <div className="ph-hero">
          <div className="ph-eyebrow">
            <span>Welcome back</span>
            <span className="sep">·</span>
            <span>{dayTime}</span>
          </div>
          <div className="ph-hero-row">
            <div className="ph-hero-text">
              <h1 className="ph-h-hero">
                Refine prompts <span className="dim">in place.</span>
              </h1>
              <p className="ph-sub">
                Select rough text anywhere on your computer, press your hotkey,
                and get a precise rewrite back in your selection.
              </p>
              <div className="ph-hero-cta-row">
                <button
                  className="ph-btn ph-btn-primary"
                  onClick={() => onNavigate("projects")}
                >
                  <FolderIcon />
                  Add project context
                </button>
              </div>
            </div>

            {/* Top-right setup card. Sits in the empty hero space and
                routes to Settings for the API-key flow. */}
            <div className="ph-setup-card">
              <div className="ph-setup-eyebrow">
                <KeyIcon />
                <span>Setup</span>
              </div>
              <div className="ph-setup-title">Set up your API</div>
              <p className="ph-setup-sub">
                Connect your API key to start enhancing prompts instantly.
              </p>
              <button
                className="ph-btn ph-btn-primary ph-setup-cta"
                onClick={() => onNavigate("settings")}
                type="button"
              >
                Set Up Now
                <ArrowRight />
              </button>
            </div>
          </div>
        </div>

        {/* ===== DEMO CARD =====
            Ported from docs/index.html's `.flow-stage` (the website's
            "promptforge · live flow" animation). Markup mirrors the
            website 1:1; styles are in Home.css scaled to this card. */}
        <div className="ph-demo-card">
          <div className="ph-demo-bar">
            <div className="ph-demo-lights">
              <span /><span /><span />
            </div>
            <span className="ph-demo-lbl">promptforge · live flow</span>
            <div className="ph-stage-progress" aria-hidden>
              <span className="ph-pdot p1" />
              <span className="ph-pdot p2" />
              <span className="ph-pdot p3" />
              <span className="ph-pdot p4" />
            </div>
            <button
              className="ph-demo-replay"
              onClick={() => setDemoKey((k) => k + 1)}
              type="button"
            >
              <ReplayIcon />
              Replay
            </button>
          </div>

          {/* Re-keyed every cycle so child CSS animations restart from t=0.
              Same trick the website uses via cloneNode. */}
          <div className="ph-stage-body" key={demoKey}>

            {/* pane 1: rough input + hotkey */}
            <div className="ph-stage-pane">
              <div className="ph-pane-label">
                <span className="num">1</span> Rough prompt
              </div>
              <div className="ph-rough-editor">
                <span className="line">
                  <span className="ph-selection-wrap">
                    <span className="ph-typed">email an angel investor i found on linkedin</span>
                  </span>
                </span>
              </div>
              <div className="ph-hotkey-card fires">
                <div className="ph-hlabel">Press the hotkey</div>
                <div className="ph-hotkey-keys">
                  {hotkeyParts.map((part, i) => (
                    <span key={i} className="ph-hotkey-keypair">
                      <span className={`ph-hkey k${i + 1}`}>{part}</span>
                      {i < hotkeyParts.length - 1 && (
                        <span className="ph-hkey-plus">+</span>
                      )}
                    </span>
                  ))}
                </div>
              </div>
              <div className="ph-pane-connector c1" />
            </div>

            {/* pane 2: smart question engine */}
            <div className="ph-stage-pane ph-question-pane">
              <div className="ph-pane-label">
                <span className="num">2</span> Smart Question Engine
              </div>
              <div className="ph-qcard">
                <div className="ph-qcard-title">What stage are you?</div>
                <div className="ph-qchips">
                  <span className="ph-qchip">Idea</span>
                  <span className="ph-qchip">Building</span>
                  <span className="ph-qchip picked">Early users</span>
                  <span className="ph-qchip">Revenue</span>
                </div>

                <div className="ph-qcard-title">Connection to them?</div>
                <div className="ph-qchips q2">
                  <span className="ph-qchip">Mutual</span>
                  <span className="ph-qchip picked">Their content</span>
                  <span className="ph-qchip">Cold</span>
                </div>

                <div className="ph-qcard-foot">
                  <div className="ph-qfoot-hint">
                    <kbd>↵</kbd> submit · <kbd>esc</kbd> skip
                  </div>
                  <button className="ph-qsubmit firing" type="button">
                    Enhance →
                  </button>
                </div>
              </div>
              <div className="ph-pane-connector c2" />
            </div>

            {/* pane 3: enhanced output */}
            <div className="ph-stage-pane ph-output-pane">
              <div className="ph-pane-label">
                <span className="num">3</span> Enhanced prompt
              </div>

              <div className="ph-thinking-pill">
                <span className="ph-spin" />
                Enhancing · 1.8s
              </div>

              <div className="ph-output-body">
                <p className="h-line">Write a short cold outreach email to an angel investor who invests in developer tools.</p>
                <p>Live product, early users, no revenue yet. Reference one specific post of theirs naturally.</p>
                <p>Goal: their feedback — not a pitch. Keep under 120 words.</p>
                <p>End with a specific ask: 15 minutes, their preferred format.</p>
              </div>
            </div>

          </div>
        </div>

        {/* ===== STATUS STRIP ===== */}
        <div className="ph-strip">
          <button
            className="ph-strip-card cta"
            onClick={() => onNavigate("settings")}
            type="button"
          >
            <div className="ph-stl">Hotkey</div>
            <div className="ph-value-keys">
              {hotkeyParts.map((part, i) => (
                <span key={i} className="ph-keypair">
                  <kbd className="ph-kbd">{part}</kbd>
                  {i < hotkeyParts.length - 1 && <span className="ph-kbd-plus">+</span>}
                </span>
              ))}
            </div>
            <div className="ph-strip-footer">
              <span>{enabled ? "System-wide" : "Paused"}</span>
              <span className="action">Customize →</span>
            </div>
          </button>

          <button
            className={`ph-strip-card ${hasKey ? "connected" : "cta empty"}`}
            onClick={() => onNavigate("settings")}
            type="button"
          >
            <div className="ph-stl">API key</div>
            <div className="ph-value">{hasKey ? "Connected" : "Missing"}</div>
            <div className="ph-strip-footer">
              <span>
                {keyStatus?.from_env
                  ? "from .env"
                  : keyStatus?.from_settings
                    ? "from settings"
                    : "not configured"}
              </span>
              <span className="action">Manage →</span>
            </div>
          </button>

          <button
            className={`ph-strip-card cta ${active ? "" : "empty"}`}
            onClick={() => onNavigate("projects")}
            type="button"
          >
            <div className="ph-stl">Active project</div>
            <div className="ph-value">{active?.name ?? "none"}</div>
            <div className="ph-strip-footer">
              <span>{active ? "context attached" : "No context attached"}</span>
              <span className="action">{active ? "Change →" : "Attach →"}</span>
            </div>
          </button>
        </div>

        {/* ===== RECENT ENHANCEMENTS ===== */}
        <div className="ph-recent-section">
          <div className="ph-recent-head">
            <div>
              <div className="ph-eyebrow flame">Recent enhancements</div>
              <h2 className="ph-h-section">
                Today's work, <span className="dim">at a glance.</span>
              </h2>
            </div>
            <div className="ph-recent-activity" aria-label="7-day activity">
              <div className="ph-activity-row">
                {SAMPLE_ACTIVITY.map((level, i) => (
                  <div
                    key={i}
                    className={`ph-a-day ${level} ${
                      i === SAMPLE_ACTIVITY.length - 1 ? "today" : ""
                    }`}
                  />
                ))}
              </div>
              <div className="ph-activity-labels">
                <span>T</span><span>W</span><span>T</span>
                <span>F</span><span>S</span><span>S</span><span>M</span>
              </div>
            </div>
          </div>

          <div className="ph-recent-list">
            {SAMPLE_RECENT.map((r, i) => (
              <div key={i} className="ph-recent-row">
                <div className="ph-recent-time">{r.time}</div>
                <div className="ph-recent-preview">
                  <span className="from">{r.from}</span>
                  <span className="arrow">→</span>
                  <span className="to">{r.to}</span>
                </div>
                <div className="ph-recent-action">
                  Open
                  <ArrowRight size={11} />
                </div>
              </div>
            ))}
          </div>
        </div>

        {!hasKey && (
          <div className="ph-banner">
            <div>
              <strong>Add a Groq API key to start.</strong>
              <p>Free at console.groq.com — takes about 30 seconds.</p>
            </div>
            <button className="ph-btn ph-btn-primary" onClick={() => onNavigate("settings")}>
              Open Settings
            </button>
          </div>
        )}
      </div>
    </div>
  );
}

/* ---------- inline SVG icons ---------- */

function ArrowRight({ size = 13 }: { size?: number }) {
  return (
    <svg viewBox="0 0 24 24" width={size} height={size} fill="none" stroke="currentColor" strokeWidth="2.4" aria-hidden="true">
      <path d="M5 12h14M13 6l6 6-6 6" />
    </svg>
  );
}

function FolderIcon() {
  return (
    <svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" strokeWidth="1.6" aria-hidden="true">
      <path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v9a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z" />
    </svg>
  );
}

function KeyIcon() {
  return (
    <svg viewBox="0 0 24 24" width="12" height="12" fill="none" stroke="currentColor" strokeWidth="1.8" aria-hidden="true">
      <circle cx="8" cy="15" r="4" />
      <path d="M10.85 12.15 19 4" />
      <path d="m18 5 2 2" />
      <path d="m15 8 2 2" />
    </svg>
  );
}

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

function ReplayIcon() {
  return (
    <svg viewBox="0 0 24 24" width="11" height="11" fill="none" stroke="currentColor" strokeWidth="1.8" aria-hidden="true">
      <path d="M3 12a9 9 0 1 0 3-6.7L3 8" />
      <path d="M3 3v5h5" />
    </svg>
  );
}
