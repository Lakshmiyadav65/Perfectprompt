import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { openUrl } from "@tauri-apps/plugin-opener";
import { ApiKeySetupChecklist } from "./ApiKeySetupChecklist";
import type { FocusTarget } from "./Shell";
import "./Settings.css";

type ApiKeyStatus = {
  from_env: boolean;
  from_settings: boolean;
  last_test_passed: boolean;
};
type UpdateInfo = {
  current_version: string;
  latest_version: string;
  update_available: boolean;
  release_url: string;
  release_notes: string | null;
};
// Discriminated union for the Updates section so each render state owns
// exactly the data it needs. Beats a (info | msg | busy) triple because
// you can't accidentally show "no update" and an error at the same time.
type UpdateState =
  | { kind: "idle" }
  | { kind: "checking" }
  | { kind: "no-update"; current: string }
  | { kind: "available"; info: UpdateInfo }
  | { kind: "error" };
type Msg = { ok: boolean; text: string } | null;

type AppClassification = "developer" | "general";
type AppClassificationSettings = {
  developer_apps: string[];
  general_apps: string[];
  default_unknown_app_behavior: AppClassification;
  use_project_awareness_in_developer_apps: boolean;
};
type DefaultClassificationLists = {
  developer_apps: string[];
  general_apps: string[];
};

interface SettingsProps {
  /// When set, Settings scrolls + focuses the matching section on
  /// mount. Caller (Shell) is responsible for clearing it via
  /// onFocusHandled once we've reacted, so the highlight doesn't
  /// re-fire on subsequent renders.
  focusTarget?: FocusTarget;
  onFocusHandled?: () => void;
}

export function Settings({ focusTarget, onFocusHandled }: SettingsProps = {}) {
  const [keyStatus, setKeyStatus] = useState<ApiKeyStatus | null>(null);
  const [hotkey, setHotkey] = useState("Alt+E");
  const [hotkeyMsg, setHotkeyMsg] = useState<Msg>(null);
  const [recording, setRecording] = useState(false);
  const [busy, setBusy] = useState<string | null>(null);
  const [updateState, setUpdateState] = useState<UpdateState>({ kind: "idle" });
  const [appClass, setAppClass] = useState<AppClassificationSettings>({
    developer_apps: [],
    general_apps: [],
    default_unknown_app_behavior: "general",
    use_project_awareness_in_developer_apps: true,
  });
  const [appClassDefaults, setAppClassDefaults] =
    useState<DefaultClassificationLists | null>(null);
  const [newDevApp, setNewDevApp] = useState("");
  const [newGeneralApp, setNewGeneralApp] = useState("");
  const [appClassMsg, setAppClassMsg] = useState<Msg>(null);
  const apiKeySectionRef = useRef<HTMLElement>(null);
  const apiKeyInputRef = useRef<HTMLInputElement>(null);
  const [apiKeyHighlighted, setApiKeyHighlighted] = useState(false);

  useEffect(() => {
    refresh();
  }, []);

  // Cross-screen focus handler. When Shell tells us the user arrived
  // via an "api-key" CTA (Home setup card, banner, or status tile),
  // scroll the section into view, pulse a brief highlight ring, and
  // focus the input so they can paste immediately.
  useEffect(() => {
    if (focusTarget !== "api-key") return;
    // Defer to the next frame so the section has rendered before we
    // scroll/focus — otherwise scrollIntoView fires against a layout
    // that hasn't laid out yet on first mount.
    const id = window.requestAnimationFrame(() => {
      apiKeySectionRef.current?.scrollIntoView({
        behavior: "smooth",
        block: "start",
      });
      apiKeyInputRef.current?.focus();
      setApiKeyHighlighted(true);
      window.setTimeout(() => setApiKeyHighlighted(false), 1600);
      onFocusHandled?.();
    });
    return () => window.cancelAnimationFrame(id);
  }, [focusTarget, onFocusHandled]);

  async function refresh() {
    try {
      const status = await invoke<ApiKeyStatus>("api_key_status");
      setKeyStatus(status);
      const hk = await invoke<string>("get_hotkey");
      setHotkey(hk);
      const ac = await invoke<AppClassificationSettings>(
        "get_app_classification_settings",
      );
      setAppClass(ac);
      const defaults = await invoke<DefaultClassificationLists>(
        "get_default_classification_lists",
      );
      setAppClassDefaults(defaults);
    } catch (e) {
      console.error("refresh failed:", e);
    }
  }

  async function persistAppClass(next: AppClassificationSettings) {
    setBusy("appclass");
    setAppClassMsg(null);
    try {
      await invoke("save_app_classification_settings", { payload: next });
      setAppClass(next);
      setAppClassMsg({ ok: true, text: "Saved." });
    } catch (e) {
      setAppClassMsg({ ok: false, text: String(e) });
    } finally {
      setBusy(null);
    }
  }

  function addAppToList(list: "developer_apps" | "general_apps", entry: string) {
    const trimmed = entry.trim();
    if (!trimmed) return;
    const lower = trimmed.toLowerCase();
    const existing = appClass[list].map((s) => s.toLowerCase());
    if (existing.includes(lower)) return;
    void persistAppClass({
      ...appClass,
      [list]: [...appClass[list], trimmed],
    });
  }

  function removeAppFromList(
    list: "developer_apps" | "general_apps",
    entry: string,
  ) {
    void persistAppClass({
      ...appClass,
      [list]: appClass[list].filter((s) => s !== entry),
    });
  }

  // saveKey / clearKey / testConnection used to live here. They were
  // moved into ApiKeySetupChecklist so the checklist is the single
  // source of truth for API-key state. Settings.tsx now just hosts
  // the checklist plus the unrelated sections (hotkey, engine, etc.).

  async function checkForUpdates() {
    setUpdateState({ kind: "checking" });
    try {
      const info = await invoke<UpdateInfo>("check_for_updates");
      if (info.update_available) {
        setUpdateState({ kind: "available", info });
      } else {
        setUpdateState({ kind: "no-update", current: info.current_version });
      }
    } catch (e) {
      // Log the raw error for debugging but show the user a clean
      // "couldn't check" message — GitHub rate-limits, DNS failures,
      // and prerelease-only repos all produce technical strings the
      // user can't act on.
      console.error("check_for_updates failed:", e);
      setUpdateState({ kind: "error" });
    }
  }

  async function saveHotkey() {
    setBusy("hotkey");
    setHotkeyMsg(null);
    try {
      await invoke("save_hotkey", { combo: hotkey });
      setHotkeyMsg({ ok: true, text: `Registered ${hotkey}` });
    } catch (e) {
      setHotkeyMsg({ ok: false, text: String(e) });
    } finally {
      setBusy(null);
    }
  }

  // Capture-on-keydown for the hotkey input
  useEffect(() => {
    if (!recording) return;
    const handler = (e: KeyboardEvent) => {
      e.preventDefault();
      e.stopPropagation();

      const parts: string[] = [];
      if (e.ctrlKey || e.metaKey) parts.push("CommandOrControl");
      if (e.altKey) parts.push("Alt");
      if (e.shiftKey) parts.push("Shift");

      const k = e.key;
      if (["Control", "Alt", "Shift", "Meta", "OS", "Hyper"].includes(k)) {
        return; // wait for non-modifier
      }
      const keyName = k.length === 1 ? k.toUpperCase() : k;
      parts.push(keyName);
      setHotkey(parts.join("+"));
      setRecording(false);
    };
    window.addEventListener("keydown", handler, true);
    return () => window.removeEventListener("keydown", handler, true);
  }, [recording]);

  if (!keyStatus) {
    return (
      <div className="pf-settings">
        <p>Loading…</p>
      </div>
    );
  }

  return (
    <div className="pf-settings">
      <h1>PerfectPrompt Settings</h1>

      <ApiKeySetupChecklist
        keyStatus={keyStatus}
        onKeyStatusChange={refresh}
        sectionRef={apiKeySectionRef}
        apiKeyInputRef={apiKeyInputRef}
        highlightClass={apiKeyHighlighted ? "pf-section-highlight" : ""}
      />



      <section>
        <h2>Global Hotkey</h2>
        <p className="pf-hint">
          Click the field, then press the combo you want. Save re-registers it
          system-wide.
        </p>
        <div className="pf-row">
          <input
            value={recording ? "Press a key combo…" : hotkey}
            readOnly
            onFocus={() => setRecording(true)}
            onBlur={() => setRecording(false)}
            className={recording ? "pf-recording" : ""}
            disabled={busy === "hotkey"}
          />
          <button onClick={saveHotkey} disabled={busy === "hotkey" || recording}>
            Save
          </button>
        </div>
        {hotkeyMsg && (
          <p className={hotkeyMsg.ok ? "pf-msg pf-ok" : "pf-msg pf-err"}>
            {hotkeyMsg.text}
          </p>
        )}
      </section>

      <section>
        <h2>Active-App Routing</h2>
        <p className="pf-hint">
          When the hotkey fires inside a developer environment (VS Code,
          Cursor, terminals, JetBrains IDEs, …) PerfectPrompt skips the
          questionnaire and enhances directly using your active project
          context. Everywhere else it shows the guided questionnaire.
        </p>

        <div className="pf-row" style={{ marginTop: 8, alignItems: "flex-start" }}>
          <label
            style={{
              display: "flex",
              gap: 8,
              alignItems: "flex-start",
              fontSize: 12,
              color: "#c8c8cc",
              lineHeight: 1.4,
            }}
          >
            <input
              type="checkbox"
              checked={appClass.use_project_awareness_in_developer_apps}
              disabled={busy === "appclass"}
              onChange={(e) =>
                void persistAppClass({
                  ...appClass,
                  use_project_awareness_in_developer_apps: e.target.checked,
                })
              }
            />
            <span>
              Inject the active project's description and links into
              developer-direct enhancements.
            </span>
          </label>
        </div>

        <div style={{ marginTop: 14 }}>
          <p className="pf-hint" style={{ marginBottom: 4 }}>
            Default for unrecognised apps:
          </p>
          <div className="pf-segmented" role="radiogroup" aria-label="Unknown app behavior">
            <button
              type="button"
              role="radio"
              aria-checked={appClass.default_unknown_app_behavior === "general"}
              className={`pf-segment ${
                appClass.default_unknown_app_behavior === "general" ? "selected" : ""
              }`}
              disabled={busy === "appclass"}
              onClick={() =>
                void persistAppClass({
                  ...appClass,
                  default_unknown_app_behavior: "general",
                })
              }
            >
              Show questionnaire
            </button>
            <button
              type="button"
              role="radio"
              aria-checked={appClass.default_unknown_app_behavior === "developer"}
              className={`pf-segment ${
                appClass.default_unknown_app_behavior === "developer" ? "selected" : ""
              }`}
              disabled={busy === "appclass"}
              onClick={() =>
                void persistAppClass({
                  ...appClass,
                  default_unknown_app_behavior: "developer",
                })
              }
            >
              Skip questionnaire
            </button>
          </div>
        </div>

        <div style={{ marginTop: 14 }}>
          <p className="pf-hint" style={{ marginBottom: 4 }}>
            Always treat as <strong>developer</strong> (skip questionnaire):
          </p>
          <AppList
            entries={appClass.developer_apps}
            onRemove={(e) => removeAppFromList("developer_apps", e)}
            disabled={busy === "appclass"}
          />
          <div className="pf-row" style={{ marginTop: 6 }}>
            <input
              placeholder="e.g. Code.exe"
              value={newDevApp}
              onChange={(e) => setNewDevApp(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") {
                  addAppToList("developer_apps", newDevApp);
                  setNewDevApp("");
                }
              }}
              disabled={busy === "appclass"}
            />
            <button
              onClick={() => {
                addAppToList("developer_apps", newDevApp);
                setNewDevApp("");
              }}
              disabled={busy === "appclass" || !newDevApp.trim()}
            >
              Add
            </button>
          </div>
        </div>

        <div style={{ marginTop: 14 }}>
          <p className="pf-hint" style={{ marginBottom: 4 }}>
            Always treat as <strong>general</strong> (show questionnaire):
          </p>
          <AppList
            entries={appClass.general_apps}
            onRemove={(e) => removeAppFromList("general_apps", e)}
            disabled={busy === "appclass"}
          />
          <div className="pf-row" style={{ marginTop: 6 }}>
            <input
              placeholder="e.g. notepad.exe"
              value={newGeneralApp}
              onChange={(e) => setNewGeneralApp(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") {
                  addAppToList("general_apps", newGeneralApp);
                  setNewGeneralApp("");
                }
              }}
              disabled={busy === "appclass"}
            />
            <button
              onClick={() => {
                addAppToList("general_apps", newGeneralApp);
                setNewGeneralApp("");
              }}
              disabled={busy === "appclass" || !newGeneralApp.trim()}
            >
              Add
            </button>
          </div>
        </div>

        {appClassDefaults && (
          <details style={{ marginTop: 14 }}>
            <summary
              style={{
                fontSize: 12,
                color: "#9c9ca0",
                cursor: "pointer",
                userSelect: "none",
              }}
            >
              Show built-in recognised apps ({appClassDefaults.developer_apps.length}{" "}
              dev / {appClassDefaults.general_apps.length} general)
            </summary>
            <div style={{ marginTop: 8 }}>
              <p className="pf-hint" style={{ marginBottom: 4 }}>Built-in developer apps:</p>
              <p className="pf-sub" style={{ wordBreak: "break-word" }}>
                {appClassDefaults.developer_apps.join(", ")}
              </p>
              <p className="pf-hint" style={{ marginBottom: 4, marginTop: 8 }}>
                Built-in general apps:
              </p>
              <p className="pf-sub" style={{ wordBreak: "break-word" }}>
                {appClassDefaults.general_apps.join(", ")}
              </p>
            </div>
          </details>
        )}

        {appClassMsg && (
          <p className={appClassMsg.ok ? "pf-msg pf-ok" : "pf-msg pf-err"}>
            {appClassMsg.text}
          </p>
        )}
      </section>

      <section>
        <h2>Updates</h2>
        <p className="pf-hint">
          Check GitHub for a newer release of Perfect Prompts.
        </p>

        <div className="pf-update-card">
          {updateState.kind === "idle" && (
            <div className="pf-update-body">
              <p className="pf-update-sub">
                Click below to check for the latest release.
              </p>
              <div className="pf-update-actions">
                <button
                  className="pf-update-primary"
                  onClick={checkForUpdates}
                >
                  Check for updates
                </button>
              </div>
            </div>
          )}

          {updateState.kind === "checking" && (
            <div className="pf-update-body pf-update-checking">
              <span className="pf-update-spinner" aria-hidden="true" />
              <span>Checking for updates…</span>
            </div>
          )}

          {updateState.kind === "no-update" && (
            <div className="pf-update-body">
              <p className="pf-update-title">No updates available.</p>
              <p className="pf-update-sub">
                You're using the latest version (v{updateState.current}).
              </p>
              <div className="pf-update-actions">
                <button
                  className="pf-update-secondary"
                  onClick={checkForUpdates}
                >
                  Check again
                </button>
              </div>
            </div>
          )}

          {updateState.kind === "available" && (
            <div className="pf-update-body">
              <p className="pf-update-title pf-update-title-accent">
                Update available
              </p>
              <div className="pf-update-versions">
                <div className="pf-update-version">
                  <span className="pf-update-vlabel">Current</span>
                  <span className="pf-update-vnum">
                    v{updateState.info.current_version}
                  </span>
                </div>
                <span className="pf-update-arrow" aria-hidden="true">
                  →
                </span>
                <div className="pf-update-version pf-update-version-new">
                  <span className="pf-update-vlabel">Latest</span>
                  <span className="pf-update-vnum">
                    v{updateState.info.latest_version}
                  </span>
                </div>
              </div>
              {updateState.info.release_notes &&
                updateState.info.release_notes.trim() && (
                  <div className="pf-update-notes">
                    <p className="pf-update-notes-label">Release notes</p>
                    <pre className="pf-update-notes-body">
                      {updateState.info.release_notes.trim()}
                    </pre>
                  </div>
                )}
              <div className="pf-update-actions">
                <button
                  className="pf-update-primary"
                  onClick={() => {
                    openUrl(updateState.info.release_url).catch(
                      console.error,
                    );
                  }}
                >
                  Update now
                </button>
                <button
                  className="pf-update-secondary"
                  onClick={checkForUpdates}
                >
                  Check again
                </button>
              </div>
            </div>
          )}

          {updateState.kind === "error" && (
            <div className="pf-update-body">
              <p className="pf-update-title pf-update-title-error">
                Couldn't check for updates.
              </p>
              <p className="pf-update-sub">Please try again.</p>
              <div className="pf-update-actions">
                <button
                  className="pf-update-secondary"
                  onClick={checkForUpdates}
                >
                  Try again
                </button>
              </div>
            </div>
          )}
        </div>
      </section>
    </div>
  );
}

function AppList({
  entries,
  onRemove,
  disabled,
}: {
  entries: string[];
  onRemove: (entry: string) => void;
  disabled: boolean;
}) {
  if (entries.length === 0) {
    return (
      <p className="pf-sub" style={{ marginTop: 0 }}>
        (no overrides — built-in defaults apply)
      </p>
    );
  }
  return (
    <div style={{ display: "flex", flexWrap: "wrap", gap: 6, marginTop: 4 }}>
      {entries.map((entry) => (
        <span
          key={entry}
          style={{
            display: "inline-flex",
            alignItems: "center",
            gap: 6,
            padding: "4px 8px",
            background: "#2a2a2c",
            border: "1px solid #3a3a3c",
            borderRadius: 6,
            fontSize: 12,
            color: "#e6e6e6",
          }}
        >
          <span style={{ fontFamily: "ui-monospace, Menlo, monospace" }}>
            {entry}
          </span>
          <button
            onClick={() => onRemove(entry)}
            disabled={disabled}
            aria-label={`Remove ${entry}`}
            style={{
              background: "transparent",
              border: "none",
              color: "#aaa",
              cursor: disabled ? "not-allowed" : "pointer",
              fontSize: 14,
              lineHeight: 1,
              padding: 0,
            }}
          >
            ×
          </button>
        </span>
      ))}
    </div>
  );
}
