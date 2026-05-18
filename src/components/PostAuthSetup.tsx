import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useAuth } from "../hooks/useAuth";
import { useDisplayName } from "../hooks/useDisplayName";
import "./SignupGate.css";

interface ApiKeyStatus {
  from_env: boolean;
  from_settings: boolean;
}

/// One-shot interstitial that runs immediately after a fresh sign-in
/// or sign-up. Always shows up after auth — the user explicitly asked
/// for a "you successfully logged in" confirmation page. The API-key
/// CTA is contextual within that page: prompted when the key is
/// missing, replaced with a plain Continue button when it's already
/// configured.
export function PostAuthSetup() {
  const auth = useAuth();
  const displayName = useDisplayName(auth.user);
  const [hasKey, setHasKey] = useState<boolean | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    let cancelled = false;
    invoke<ApiKeyStatus>("api_key_status")
      .then((status) => {
        if (cancelled) return;
        setHasKey(status.from_env || status.from_settings);
      })
      .catch((e) => {
        console.error("[post-auth] api_key_status failed:", e);
        if (!cancelled) setHasKey(false);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  // Brief loading frame while the api-key check resolves — keeps the
  // UI from flickering between "set up your key" and "you're all set"
  // depending on which state lands first.
  if (hasKey === null) return null;

  function handleSetupKey() {
    if (busy) return;
    setBusy(true);
    // Drop a breadcrumb that Shell reads on mount so it pre-routes to
    // Settings → API Key with the focus highlight. sessionStorage is
    // the cleanest hand-off here: the value survives the unmount of
    // this component and the mount of Shell, then we clear it.
    try {
      sessionStorage.setItem("pf:post-auth-focus", "api-key");
    } catch (e) {
      console.warn("[post-auth] sessionStorage unavailable:", e);
    }
    auth.dismissJustSignedIn();
  }

  function handleContinue() {
    if (busy) return;
    setBusy(true);
    auth.dismissJustSignedIn();
  }

  const firstName = displayName.defaultName
    ? displayName.defaultName.split(/\s+/)[0]
    : null;

  return (
    <div className="pf-gate">
      <div className="pf-gate-card">
        <div className="pf-gate-brand">
          <div className="pf-gate-brand-mark" aria-hidden="true" />
          <span className="pf-gate-brand-name">PerfectPrompt</span>
        </div>

        <div className="pf-gate-success-badge">
          <CheckMark />
          <span>Signed in</span>
        </div>

        <h1 className="pf-gate-title">
          {firstName ? `Welcome, ${firstName}.` : "You're in."}
        </h1>

        {hasKey ? (
          <>
            <p className="pf-gate-sub">
              Your Groq API key is already connected. You're ready to
              start enhancing prompts from anywhere with your hotkey.
            </p>
            <div className="pf-gate-actions">
              <button
                type="button"
                className="pf-gate-btn pf-gate-btn-primary"
                onClick={handleContinue}
                disabled={busy}
              >
                Continue to PerfectPrompt
              </button>
            </div>
          </>
        ) : (
          <>
            <p className="pf-gate-sub">
              One last step — connect your free Groq API key so you can
              enhance prompts from anywhere with your hotkey. It takes
              about 30 seconds at console.groq.com.
            </p>
            <div className="pf-gate-actions">
              <button
                type="button"
                className="pf-gate-btn pf-gate-btn-primary"
                onClick={handleSetupKey}
                disabled={busy}
              >
                Set up Groq API key
              </button>
              <button
                type="button"
                className="pf-gate-btn"
                onClick={handleContinue}
                disabled={busy}
              >
                Skip for now
              </button>
            </div>
            <p className="pf-gate-fineprint">
              You can also use the hosted tier (50 enhancements/day)
              without a key — set up later from Settings whenever
              you're ready.
            </p>
          </>
        )}
      </div>
    </div>
  );
}

function CheckMark() {
  return (
    <svg
      viewBox="0 0 24 24"
      width="12"
      height="12"
      fill="none"
      stroke="currentColor"
      strokeWidth="2.5"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
    >
      <path d="M20 6 9 17l-5-5" />
    </svg>
  );
}
