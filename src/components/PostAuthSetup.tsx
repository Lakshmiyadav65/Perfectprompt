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
/// or sign-up. If the user already has an API key configured, this
/// component auto-dismisses so returning users go straight to Home.
/// If no key is set yet, it shows a "Signed in — connect your Groq
/// key" card with a CTA that hands off to Shell to land them on the
/// API Key section in Settings.
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
        const present = status.from_env || status.from_settings;
        setHasKey(present);
        if (present) {
          // Returning user — no API setup step needed. Dismiss
          // immediately so MainAppGated re-renders into Shell.
          auth.dismissJustSignedIn();
        }
      })
      .catch((e) => {
        console.error("[post-auth] api_key_status failed:", e);
        if (!cancelled) setHasKey(false);
      });
    return () => {
      cancelled = true;
    };
    // We intentionally do not depend on auth — calling
    // dismissJustSignedIn is a one-shot side-effect; re-running on
    // identity changes would loop.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // While the api-key check is in flight, render nothing rather than
  // flash the interstitial at a returning user with a configured key.
  if (hasKey === null || hasKey) return null;

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

  function handleSkip() {
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
            onClick={handleSkip}
            disabled={busy}
          >
            Skip for now
          </button>
        </div>

        <p className="pf-gate-fineprint">
          You can also use the hosted tier (50 enhancements/day) without
          a key — set up later from Settings whenever you're ready.
        </p>
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
