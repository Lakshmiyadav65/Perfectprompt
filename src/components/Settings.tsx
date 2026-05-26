import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { openUrl } from "@tauri-apps/plugin-opener";
import { check } from "@tauri-apps/plugin-updater";
import { relaunch } from "@tauri-apps/plugin-process";
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
    } catch (e) {
      console.error("refresh failed:", e);
    }
  }

  // saveKey / clearKey / testConnection used to live here. They were
  // moved into ApiKeySetupChecklist so the checklist is the single
  // source of truth for API-key state. Settings.tsx now just hosts
  // the checklist plus the unrelated sections (hotkey, updates).

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
                  onClick={async () => {
                    // Same plugin-based silent-install flow as the
                    // bottom-right toast banner. v0.4.8 had this
                    // button open the browser to /download which
                    // forced a manual installer-wizard run; v0.5.2
                    // unifies the two surfaces so clicking Update
                    // now from either place does the same thing.
                    try {
                      const update = await check();
                      if (!update) return;
                      await update.downloadAndInstall();
                      await relaunch();
                    } catch (e) {
                      console.error("[settings] update failed:", e);
                      // Fall back to the browser download so the
                      // user has a path forward if the plugin can't
                      // reach the manifest (network, transient
                      // Vercel hiccup, etc.).
                      openUrl(
                        "https://perfectprompt-beta.vercel.app/download",
                      ).catch(console.error);
                    }
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

      <section>
        <h2>Need help?</h2>
        <p className="pf-hint">
          Paid for Pro but the app still shows Free? Or anything else not
          working as expected? Email us with a screenshot of the issue and
          your Razorpay payment ID — we usually respond within a few hours.
        </p>
        <div className="pf-support-card">
          <div className="pf-support-row">
            <span className="pf-support-label">Support email</span>
            <span className="pf-support-email">{SUPPORT_EMAIL}</span>
          </div>
          <button
            type="button"
            className="pf-support-button"
            onClick={() => {
              const subject = encodeURIComponent(
                "PerfectPrompt support",
              );
              const body = encodeURIComponent(
                "Hi,\n\n" +
                  "Please attach a screenshot of the issue and fill in the details below:\n\n" +
                  "Account email (the Google/GitHub email you sign in with): \n" +
                  "Razorpay payment ID (if this is about a payment): \n" +
                  "What's happening: \n\n" +
                  "Thanks!",
              );
              openUrl(
                `mailto:${SUPPORT_EMAIL}?subject=${subject}&body=${body}`,
              ).catch(console.error);
            }}
          >
            Email support
          </button>
        </div>
      </section>
    </div>
  );
}

/// Support inbox for paid-but-stuck-on-free, refund requests, bug
/// reports, etc. Kept as a module-level constant so the same address
/// is reused by the mailto URL and the visible "Support email" row —
/// changing it once updates both.
const SUPPORT_EMAIL = "lakshmibeenhere@gmail.com";

