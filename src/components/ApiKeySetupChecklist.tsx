import { RefObject, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { openUrl } from "@tauri-apps/plugin-opener";
import "./ApiKeySetupChecklist.css";

const GROQ_KEYS_URL = "https://console.groq.com/keys";

interface ApiKeyStatus {
  from_env: boolean;
  from_settings: boolean;
  /// Persisted outcome of the most recent test_connection call.
  /// Lets step 3 stay ✓ across app restarts without forcing the
  /// user to re-test a key that already works.
  last_test_passed: boolean;
}

interface ConnectionTest {
  ok: boolean;
  latency_ms: number;
  message: string;
}

type Msg = { ok: boolean; text: string } | null;

interface Props {
  keyStatus: ApiKeyStatus;
  /// Called after the checklist successfully mutates key state, so
  /// the parent can re-fetch keyStatus from Rust and re-render.
  onKeyStatusChange: () => void | Promise<void>;
  /// Optional ref the parent attaches to the password input for the
  /// "scroll + focus" deep-link behavior from Home's setup card.
  apiKeyInputRef?: RefObject<HTMLInputElement | null>;
  /// Optional ref attached to the outer <section> so the parent's
  /// "scroll into view" effect lands on this card.
  sectionRef?: RefObject<HTMLElement | null>;
  /// Optional class added when the parent wants to draw attention to
  /// the section (the existing pf-section-highlight pulse).
  highlightClass?: string;
}

export function ApiKeySetupChecklist({
  keyStatus,
  onKeyStatusChange,
  apiKeyInputRef,
  sectionRef,
  highlightClass,
}: Props) {
  const [step1Clicked, setStep1Clicked] = useState(false);
  const [keyInput, setKeyInput] = useState("");
  const [busyKey, setBusyKey] = useState(false);
  const [keyMsg, setKeyMsg] = useState<Msg>(null);
  const [busyTest, setBusyTest] = useState(false);
  const [testMsg, setTestMsg] = useState<Msg>(null);
  // When the user clicks "Change" on a saved key, flip step 2 back
  // into edit mode so they can paste a new value. The previous key
  // stays active until they save the new one — escape via Cancel
  // returns to the saved state.
  const [editingKey, setEditingKey] = useState(false);

  const hasKey = keyStatus.from_env || keyStatus.from_settings;
  // Step 1 is "user has a key in hand" — we can't verify that
  // directly, but if they've either clicked the open-Groq button in
  // this session or already have a saved key, we treat it as done.
  const step1Done = step1Clicked || hasKey;
  const step2Done = hasKey;
  // Step 3 honors either the in-session test result or the persisted
  // flag from settings.json — so after a fresh restart a previously
  // verified key still reads as tested instead of dropping back to
  // pending on every launch.
  const step3Done = testMsg?.ok === true || keyStatus.last_test_passed === true;
  const completed = [step1Done, step2Done, step3Done].filter(Boolean).length;
  const allDone = completed === 3;

  // Compute the active step (next one needing user action). Used for
  // the focus ring + "active" styling on the indicator.
  const activeStep = !step1Done ? 1 : !step2Done ? 2 : !step3Done ? 3 : null;

  async function handleOpenGroq() {
    try {
      await openUrl(GROQ_KEYS_URL);
    } catch (e) {
      console.error("[setup] openUrl failed:", e);
    }
    setStep1Clicked(true);
  }

  async function handleSaveKey() {
    if (!keyInput.trim() || busyKey) return;
    setBusyKey(true);
    setKeyMsg(null);
    try {
      await invoke("save_api_key", { key: keyInput.trim() });
      setKeyInput("");
      setKeyMsg({ ok: true, text: "Saved." });
      // Any prior "Connected" result was for the previous key, not
      // this one — wipe it so step 3 reads correctly.
      setTestMsg(null);
      setEditingKey(false);
      await onKeyStatusChange();
    } catch (e) {
      setKeyMsg({ ok: false, text: friendlyError(e) });
    } finally {
      setBusyKey(false);
    }
  }

  async function handleClearKey() {
    if (busyKey) return;
    setBusyKey(true);
    setKeyMsg(null);
    try {
      await invoke("clear_api_key");
      setKeyMsg({ ok: true, text: "Key removed." });
      setTestMsg(null);
      setEditingKey(false);
      setKeyInput("");
      await onKeyStatusChange();
    } catch (e) {
      setKeyMsg({ ok: false, text: friendlyError(e) });
    } finally {
      setBusyKey(false);
    }
  }

  async function handleTestConnection() {
    if (busyTest) return;
    setBusyTest(true);
    setTestMsg(null);
    try {
      const result = await invoke<ConnectionTest>("test_connection");
      setTestMsg({
        ok: result.ok,
        text: result.ok
          ? `Connected — round-trip ${result.latency_ms}ms`
          : result.message,
      });
    } catch (e) {
      setTestMsg({ ok: false, text: friendlyError(e) });
    } finally {
      setBusyTest(false);
    }
  }

  return (
    <section
      ref={sectionRef}
      className={`pf-setup-checklist ${highlightClass ?? ""}`}
      aria-label="Groq API key setup"
    >
      <header className="pf-setup-head">
        <div>
          <h2 className="pf-setup-title">
            {allDone ? "Your Groq API key" : "Set up your Groq API key"}
          </h2>
          <p className="pf-setup-subtitle">
            {allDone
              ? "Connected, tested, ready. Change or remove the key any time below."
              : "Three short steps. Free, no credit card, takes about a minute."}
          </p>
        </div>
        <div className="pf-setup-progress" aria-label={`${completed} of 3 steps complete`}>
          <span className="pf-setup-progress-count">{completed}</span>
          <span className="pf-setup-progress-divider" aria-hidden="true">/</span>
          <span className="pf-setup-progress-total">3</span>
        </div>
      </header>

      <ol className="pf-setup-steps">
        <Step
          number={1}
          done={step1Done}
          active={activeStep === 1}
          title="Get your free Groq API key"
          desc="Sign in at console.groq.com and click Create API Key. Copy the gsk_… string it gives you."
        >
          <button
            type="button"
            className={step1Done ? "pf-setup-btn-secondary" : "pf-setup-btn-primary"}
            onClick={handleOpenGroq}
          >
            {step1Done ? "Reopen console.groq.com" : "Open console.groq.com →"}
          </button>
        </Step>

        <Step
          number={2}
          done={step2Done}
          active={activeStep === 2}
          title="Paste your key here and save"
          desc="Stored locally on this machine. Your key never leaves your device until you use a hosted enhancement."
        >
          {step2Done && !editingKey ? (
            <div className="pf-setup-step-status-row">
              <div className="pf-setup-step-status">
                {keyStatus.from_env
                  ? "✓ Using key from .env (env var takes precedence)"
                  : "✓ Key saved to settings.json"}
              </div>
              <div className="pf-setup-inline-actions">
                <button
                  type="button"
                  className="pf-setup-link-btn"
                  onClick={() => {
                    setKeyMsg(null);
                    setEditingKey(true);
                  }}
                  disabled={busyKey || keyStatus.from_env}
                  title={
                    keyStatus.from_env
                      ? "Edit the GROQ_API_KEY value in your .env file to change the env-var key."
                      : undefined
                  }
                >
                  Change API key
                </button>
                <button
                  type="button"
                  className="pf-setup-link-btn pf-setup-link-btn-danger"
                  onClick={handleClearKey}
                  disabled={busyKey || !keyStatus.from_settings}
                  title={
                    keyStatus.from_env && !keyStatus.from_settings
                      ? "Remove the .env override manually — only settings.json keys can be cleared from here."
                      : undefined
                  }
                >
                  Remove
                </button>
              </div>
            </div>
          ) : (
            <>
              <div className="pf-setup-key-row">
                <input
                  ref={apiKeyInputRef}
                  type="password"
                  placeholder="gsk_..."
                  value={keyInput}
                  onChange={(e) => setKeyInput(e.target.value)}
                  disabled={busyKey || !step1Done}
                  autoComplete="off"
                  spellCheck={false}
                />
                <button
                  type="button"
                  className="pf-setup-btn-primary"
                  onClick={handleSaveKey}
                  disabled={busyKey || !keyInput.trim()}
                >
                  {busyKey ? "Saving…" : "Save"}
                </button>
                {editingKey && (
                  <button
                    type="button"
                    className="pf-setup-btn-secondary"
                    onClick={() => {
                      setKeyInput("");
                      setKeyMsg(null);
                      setEditingKey(false);
                    }}
                    disabled={busyKey}
                  >
                    Cancel
                  </button>
                )}
              </div>
            </>
          )}
          {keyMsg && (
            <p className={keyMsg.ok ? "pf-setup-msg ok" : "pf-setup-msg err"}>
              {keyMsg.text}
            </p>
          )}
        </Step>

        <Step
          number={3}
          done={step3Done}
          active={activeStep === 3}
          title="Test your connection"
          desc="Pings Groq with the saved key to confirm everything's wired correctly."
        >
          <button
            type="button"
            className={step3Done ? "pf-setup-btn-secondary" : "pf-setup-btn-primary"}
            onClick={handleTestConnection}
            disabled={busyTest || !step2Done}
          >
            {busyTest ? "Testing…" : step3Done ? "Test again" : "Test connection"}
          </button>
          {testMsg && (
            <p className={testMsg.ok ? "pf-setup-msg ok" : "pf-setup-msg err"}>
              {testMsg.text}
            </p>
          )}
        </Step>
      </ol>

      {allDone && (
        <>
          <div className="pf-setup-done" role="status">
            <CheckMark />
            <span>All set — PerfectPrompt is ready to enhance.</span>
          </div>
          <div className="pf-setup-manage">
            <button
              type="button"
              className="pf-setup-btn-secondary"
              onClick={handleChangeFromManage}
              disabled={busyKey || keyStatus.from_env}
              title={
                keyStatus.from_env
                  ? "Edit the GROQ_API_KEY value in your .env file to change the env-var key."
                  : undefined
              }
            >
              Change API key
            </button>
            <button
              type="button"
              className="pf-setup-link-btn pf-setup-link-btn-danger"
              onClick={handleClearKey}
              disabled={busyKey || !keyStatus.from_settings}
              title={
                keyStatus.from_env && !keyStatus.from_settings
                  ? "Remove the .env override manually — only settings.json keys can be cleared from here."
                  : undefined
              }
            >
              Remove key
            </button>
          </div>
        </>
      )}
    </section>
  );

  function handleChangeFromManage() {
    setKeyMsg(null);
    setEditingKey(true);
    // Scroll step 2 into view and focus the input so the user lands
    // exactly where they need to be without hunting for the field.
    if (sectionRef?.current) {
      sectionRef.current.scrollIntoView({ behavior: "smooth", block: "start" });
    }
    // Defer the focus until after the edit-mode render commits.
    window.requestAnimationFrame(() => {
      apiKeyInputRef?.current?.focus();
    });
  }
}

interface StepProps {
  number: number;
  done: boolean;
  active: boolean;
  title: string;
  desc: string;
  children: React.ReactNode;
}

function Step({ number, done, active, title, desc, children }: StepProps) {
  return (
    <li
      className={`pf-setup-step ${done ? "done" : active ? "active" : "pending"}`}
    >
      <span className="pf-setup-indicator" aria-hidden="true">
        {done ? (
          <CheckMark />
        ) : (
          <span className="pf-setup-indicator-num">{number}</span>
        )}
      </span>
      <div className="pf-setup-step-body">
        <div className="pf-setup-step-title">{title}</div>
        <p className="pf-setup-step-desc">{desc}</p>
        <div className="pf-setup-step-action">{children}</div>
      </div>
    </li>
  );
}

function CheckMark() {
  return (
    <svg
      viewBox="0 0 24 24"
      width="14"
      height="14"
      fill="none"
      stroke="currentColor"
      strokeWidth="3"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
    >
      <path d="M20 6 9 17l-5-5" />
    </svg>
  );
}

function friendlyError(e: unknown): string {
  const raw = (e as Error)?.message ?? String(e);
  // Existing Rust commands return their errors as plain strings via
  // `Result<_, String>`. Surface them as-is, but trim verbose prefixes.
  return raw.replace(/^Error:\s*/i, "");
}
