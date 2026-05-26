import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { openUrl } from "@tauri-apps/plugin-opener";
import { check } from "@tauri-apps/plugin-updater";
import { relaunch } from "@tauri-apps/plugin-process";
import { ApiKeySetupChecklist } from "./ApiKeySetupChecklist";
import { useAuth } from "../hooks/useAuth";
import { useDisplayName } from "../hooks/useDisplayName";
import { useAvatar, fileToResizedDataUrl } from "../hooks/useAvatar";
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

  // --- Profile section state ---
  const auth = useAuth();
  const displayName = useDisplayName(auth.user);
  const avatar = useAvatar();
  // Local-only draft for the name input so users can type freely
  // without the chip flickering on every keystroke. We commit to the
  // override only on Save.
  const [nameDraft, setNameDraft] = useState<string>(displayName.override);
  const [profileMsg, setProfileMsg] = useState<Msg>(null);
  const [avatarBusy, setAvatarBusy] = useState(false);
  const profileSectionRef = useRef<HTMLElement>(null);
  const nameInputRef = useRef<HTMLInputElement>(null);
  const avatarFileInputRef = useRef<HTMLInputElement>(null);
  const [profileHighlighted, setProfileHighlighted] = useState(false);

  // Sync draft when the override changes from elsewhere (another
  // window, or after a save) — but only if the user isn't actively
  // editing, to avoid stomping mid-keystroke.
  useEffect(() => {
    setNameDraft(displayName.override);
  }, [displayName.override]);

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

  // Same scroll/focus/highlight dance for the Profile dropdown entry.
  useEffect(() => {
    if (focusTarget !== "profile") return;
    const id = window.requestAnimationFrame(() => {
      profileSectionRef.current?.scrollIntoView({
        behavior: "smooth",
        block: "start",
      });
      nameInputRef.current?.focus();
      setProfileHighlighted(true);
      window.setTimeout(() => setProfileHighlighted(false), 1600);
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

  function saveProfileName() {
    const trimmed = nameDraft.trim();
    // Empty input = clear the override (fall back to Google name).
    displayName.setOverride(trimmed);
    setProfileMsg({
      ok: true,
      text: trimmed
        ? `Saved. You'll show up as "${trimmed}".`
        : `Cleared. Showing your account name instead.`,
    });
    window.setTimeout(() => setProfileMsg(null), 2400);
  }

  async function onAvatarFile(e: React.ChangeEvent<HTMLInputElement>) {
    const file = e.target.files?.[0];
    // Reset the input so picking the same file twice still fires onChange.
    if (e.target) e.target.value = "";
    if (!file) return;
    if (!file.type.startsWith("image/")) {
      setProfileMsg({ ok: false, text: "That doesn't look like an image file." });
      return;
    }
    // ~8MB pre-resize ceiling so we don't try to decode something
    // absurd. Post-resize the stored payload is ~25-40KB regardless.
    if (file.size > 8 * 1024 * 1024) {
      setProfileMsg({ ok: false, text: "Image is too large. Try one under 8MB." });
      return;
    }
    setAvatarBusy(true);
    setProfileMsg(null);
    try {
      const dataUrl = await fileToResizedDataUrl(file);
      avatar.setAvatar(dataUrl);
      setProfileMsg({ ok: true, text: "Profile photo updated." });
      window.setTimeout(() => setProfileMsg(null), 2400);
    } catch (err) {
      console.error("avatar upload failed:", err);
      setProfileMsg({ ok: false, text: "Couldn't process that image." });
    } finally {
      setAvatarBusy(false);
    }
  }

  function clearAvatar() {
    avatar.clearAvatar();
    setProfileMsg({ ok: true, text: "Profile photo removed." });
    window.setTimeout(() => setProfileMsg(null), 2400);
  }

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

      <section
        ref={profileSectionRef}
        className={profileHighlighted ? "pf-section-highlight" : ""}
      >
        <h2>Profile</h2>
        <p className="pf-hint">
          How you appear in the sidebar. Stored on this device only — your
          account email and sign-in identity don't change.
        </p>

        <div className="pf-profile-edit">
          <div className="pf-profile-edit-avatar">
            <div className="pf-profile-edit-preview" aria-hidden="true">
              {avatar.dataUrl ? (
                <img src={avatar.dataUrl} alt="" />
              ) : (
                <span className="pf-profile-edit-initial">
                  {displayName.name.charAt(0).toUpperCase() || "?"}
                </span>
              )}
            </div>
            <div className="pf-profile-edit-avatar-actions">
              <input
                ref={avatarFileInputRef}
                type="file"
                accept="image/png,image/jpeg,image/webp,image/gif"
                onChange={onAvatarFile}
                style={{ display: "none" }}
              />
              <button
                type="button"
                onClick={() => avatarFileInputRef.current?.click()}
                disabled={avatarBusy}
              >
                {avatarBusy
                  ? "Processing…"
                  : avatar.dataUrl
                  ? "Change photo"
                  : "Upload photo"}
              </button>
              {avatar.dataUrl && (
                <button
                  type="button"
                  className="pf-profile-edit-remove"
                  onClick={clearAvatar}
                  disabled={avatarBusy}
                >
                  Remove
                </button>
              )}
            </div>
          </div>

          <div className="pf-profile-edit-name">
            <label htmlFor="pf-profile-name-input">Display name</label>
            <div className="pf-row">
              <input
                id="pf-profile-name-input"
                ref={nameInputRef}
                value={nameDraft}
                onChange={(e) => setNameDraft(e.target.value)}
                placeholder={displayName.defaultName}
                maxLength={48}
              />
              <button
                onClick={saveProfileName}
                disabled={nameDraft.trim() === displayName.override}
              >
                Save
              </button>
            </div>
            <p className="pf-hint pf-profile-default-hint">
              Leave empty to use your account name
              {displayName.defaultName !== "You" && (
                <> — currently <strong>{displayName.defaultName}</strong></>
              )}
              .
            </p>
          </div>
        </div>

        {profileMsg && (
          <p className={profileMsg.ok ? "pf-msg pf-ok" : "pf-msg pf-err"}>
            {profileMsg.text}
          </p>
        )}
      </section>

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

