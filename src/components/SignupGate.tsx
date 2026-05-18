import { useState } from "react";
import { useAuth } from "../hooks/useAuth";
import "./SignupGate.css";

export function SignupGate() {
  const auth = useAuth();
  const [busy, setBusy] = useState<"google" | "github" | null>(null);

  async function handleGoogle() {
    if (busy) return;
    setBusy("google");
    try {
      await auth.signInWithGoogle();
    } catch (e) {
      console.error("[signup-gate] Google sign-in failed:", e);
    } finally {
      setBusy(null);
    }
  }

  async function handleGitHub() {
    if (busy) return;
    setBusy("github");
    try {
      await auth.signInWithGitHub();
    } catch (e) {
      console.error("[signup-gate] GitHub sign-in failed:", e);
    } finally {
      setBusy(null);
    }
  }

  return (
    <div className="pf-gate">
      <div className="pf-gate-card">
        <div className="pf-gate-brand">
          <div className="pf-gate-brand-mark" aria-hidden="true" />
          <span className="pf-gate-brand-name">PerfectPrompt</span>
        </div>

        <h1 className="pf-gate-title">Welcome.</h1>
        <p className="pf-gate-sub">
          Sign in to get started. Your account unlocks 50 free enhancements
          per day on our hosted tier — or add your own Groq key later for
          unlimited use.
        </p>

        <div className="pf-gate-actions">
          <button
            type="button"
            className="pf-gate-btn pf-gate-btn-primary"
            onClick={handleGoogle}
            disabled={busy !== null}
          >
            <GoogleMark />
            {busy === "google" ? "Opening browser…" : "Continue with Google"}
          </button>
          <button
            type="button"
            className="pf-gate-btn"
            onClick={handleGitHub}
            disabled={busy !== null}
          >
            <GitHubMark />
            {busy === "github" ? "Opening browser…" : "Continue with GitHub"}
          </button>
        </div>

        {auth.error && (
          <p className="pf-gate-error" role="alert">
            {auth.error}
          </p>
        )}

        <p className="pf-gate-fineprint">
          By signing in you agree to enhancement requests being proxied
          through our servers to Groq. Your prompts are subject to{" "}
          <a href="https://groq.com/privacy-policy" target="_blank" rel="noreferrer">
            Groq's data policy
          </a>
          .
        </p>
      </div>
    </div>
  );
}

function GoogleMark() {
  return (
    <svg viewBox="0 0 18 18" width="16" height="16" aria-hidden="true">
      <path
        fill="#4285F4"
        d="M17.64 9.2c0-.64-.06-1.25-.16-1.84H9v3.48h4.84a4.14 4.14 0 0 1-1.8 2.72v2.26h2.92c1.7-1.57 2.68-3.88 2.68-6.62z"
      />
      <path
        fill="#34A853"
        d="M9 18c2.43 0 4.47-.8 5.96-2.18l-2.92-2.26c-.8.54-1.84.86-3.04.86-2.34 0-4.32-1.58-5.03-3.7H.96v2.32A9 9 0 0 0 9 18z"
      />
      <path
        fill="#FBBC05"
        d="M3.97 10.72A5.4 5.4 0 0 1 3.68 9c0-.6.1-1.18.29-1.72V4.96H.96A9 9 0 0 0 0 9c0 1.45.35 2.83.96 4.04l3.01-2.32z"
      />
      <path
        fill="#EA4335"
        d="M9 3.58c1.32 0 2.5.45 3.44 1.35l2.58-2.58C13.46.89 11.43 0 9 0A9 9 0 0 0 .96 4.96l3.01 2.32C4.68 5.16 6.66 3.58 9 3.58z"
      />
    </svg>
  );
}

function GitHubMark() {
  return (
    <svg viewBox="0 0 16 16" width="16" height="16" fill="currentColor" aria-hidden="true">
      <path d="M8 0C3.58 0 0 3.58 0 8a8 8 0 0 0 5.47 7.59c.4.07.55-.17.55-.38 0-.19-.01-.82-.01-1.49-2.01.37-2.53-.49-2.69-.94-.09-.23-.48-.94-.82-1.13-.28-.15-.68-.52-.01-.53.63-.01 1.08.58 1.23.82.72 1.21 1.87.87 2.33.66.07-.52.28-.87.51-1.07-1.78-.2-3.64-.89-3.64-3.95 0-.87.31-1.59.82-2.15-.08-.2-.36-1.02.08-2.12 0 0 .67-.21 2.2.82.64-.18 1.32-.27 2-.27.68 0 1.36.09 2 .27 1.53-1.04 2.2-.82 2.2-.82.44 1.1.16 1.92.08 2.12.51.56.82 1.27.82 2.15 0 3.07-1.87 3.75-3.65 3.95.29.25.54.73.54 1.48 0 1.07-.01 1.93-.01 2.2 0 .21.15.46.55.38A8.01 8.01 0 0 0 16 8c0-4.42-3.58-8-8-8z" />
    </svg>
  );
}
