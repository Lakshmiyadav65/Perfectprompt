import { FormEvent, useState } from "react";
import { useAuth } from "../hooks/useAuth";
import "./SignupGate.css";

type Mode = "signin" | "signup" | "reset";

interface FieldErrors {
  name?: string;
  email?: string;
  password?: string;
}

const EMAIL_RE = /^[^\s@]+@[^\s@]+\.[^\s@]+$/;
const MIN_PASSWORD = 6;

export function SignupGate() {
  const auth = useAuth();
  const [mode, setMode] = useState<Mode>("signin");
  const [name, setName] = useState("");
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");
  const [fieldErrors, setFieldErrors] = useState<FieldErrors>({});
  const [submitting, setSubmitting] = useState(false);
  const [oauthBusy, setOauthBusy] = useState<"google" | null>(null);
  const [emailSentTo, setEmailSentTo] = useState<string | null>(null);
  const [confirmationPending, setConfirmationPending] = useState(false);

  function switchMode(next: Mode) {
    setMode(next);
    setFieldErrors({});
    setEmailSentTo(null);
    setConfirmationPending(false);
  }

  function validate(): boolean {
    const errs: FieldErrors = {};
    if (mode === "signup" && !name.trim()) {
      errs.name = "Please enter your name.";
    }
    if (!email.trim()) {
      errs.email = "Email is required.";
    } else if (!EMAIL_RE.test(email.trim())) {
      errs.email = "Please enter a valid email.";
    }
    if (mode !== "reset") {
      if (!password) {
        errs.password = "Password is required.";
      } else if (password.length < MIN_PASSWORD) {
        errs.password = `Password must be at least ${MIN_PASSWORD} characters.`;
      }
    }
    setFieldErrors(errs);
    return Object.keys(errs).length === 0;
  }

  async function handleSubmit(e: FormEvent) {
    e.preventDefault();
    if (submitting || oauthBusy) return;
    if (!validate()) return;

    setSubmitting(true);
    try {
      if (mode === "signin") {
        await auth.signInWithPassword(email.trim(), password);
      } else if (mode === "signup") {
        const { needsEmailConfirmation } = await auth.signUpWithPassword(
          name.trim(),
          email.trim(),
          password,
        );
        if (needsEmailConfirmation) {
          setConfirmationPending(true);
        }
      } else {
        await auth.resetPasswordForEmail(email.trim());
        setEmailSentTo(email.trim());
      }
    } catch {
      // Error already surfaced via auth.error.
    } finally {
      setSubmitting(false);
    }
  }

  async function handleGoogle() {
    if (oauthBusy || submitting) return;
    setOauthBusy("google");
    try {
      await auth.signInWithGoogle();
    } catch (e) {
      console.error("[signup-gate] Google sign-in failed:", e);
    } finally {
      setOauthBusy(null);
    }
  }

  const busy = submitting || oauthBusy !== null;

  // --- post-action states (terminal screens that replace the form) ---

  if (emailSentTo) {
    return (
      <div className="pf-gate">
        <div className="pf-gate-card">
          <div className="pf-gate-brand">
            <div className="pf-gate-brand-mark" aria-hidden="true" />
            <span className="pf-gate-brand-name">PerfectPrompt</span>
          </div>
          <h1 className="pf-gate-title">Check your email</h1>
          <p className="pf-gate-sub">
            We sent a password-reset link to <strong>{emailSentTo}</strong>.
            Click the link to set a new password and sign in.
          </p>
          <button
            type="button"
            className="pf-gate-btn"
            onClick={() => switchMode("signin")}
          >
            Back to sign in
          </button>
        </div>
      </div>
    );
  }

  if (confirmationPending) {
    return (
      <div className="pf-gate">
        <div className="pf-gate-card">
          <div className="pf-gate-brand">
            <div className="pf-gate-brand-mark" aria-hidden="true" />
            <span className="pf-gate-brand-name">PerfectPrompt</span>
          </div>
          <h1 className="pf-gate-title">One more step</h1>
          <p className="pf-gate-sub">
            We sent a confirmation link to <strong>{email.trim()}</strong>.
            Click the link to activate your account, then come back here to
            sign in.
          </p>
          <button
            type="button"
            className="pf-gate-btn"
            onClick={() => switchMode("signin")}
          >
            Back to sign in
          </button>
        </div>
      </div>
    );
  }

  // --- main form ---

  const titles: Record<Mode, { title: string; sub: string; cta: string }> = {
    signin: {
      title: "Welcome back",
      sub: "Sign in to continue using PerfectPrompts.",
      cta: "Sign in",
    },
    signup: {
      title: "Create your account",
      sub: "Start enhancing your writing instantly with PerfectPrompts.",
      cta: "Sign up",
    },
    reset: {
      title: "Reset your password",
      sub: "Enter the email tied to your account and we'll send you a reset link.",
      cta: "Send reset link",
    },
  };
  const t = titles[mode];

  return (
    <div className="pf-gate">
      <div className="pf-gate-card">
        <div className="pf-gate-brand">
          <div className="pf-gate-brand-mark" aria-hidden="true" />
          <span className="pf-gate-brand-name">PerfectPrompt</span>
        </div>

        <h1 className="pf-gate-title">{t.title}</h1>
        <p className="pf-gate-sub">{t.sub}</p>

        {mode !== "reset" && (
          <>
            <div className="pf-gate-actions">
              <button
                type="button"
                className="pf-gate-btn"
                onClick={handleGoogle}
                disabled={busy}
              >
                <GoogleMark />
                {oauthBusy === "google" ? "Opening browser…" : "Continue with Google"}
              </button>
            </div>

            <div className="pf-gate-divider" aria-hidden="true">
              <span>or</span>
            </div>
          </>
        )}

        <form className="pf-gate-form" onSubmit={handleSubmit} noValidate>
          {mode === "signup" && (
            <Field
              id="pf-gate-name"
              label="Name"
              type="text"
              autoComplete="name"
              value={name}
              onChange={setName}
              error={fieldErrors.name}
              disabled={busy}
            />
          )}
          <Field
            id="pf-gate-email"
            label="Email"
            type="email"
            autoComplete={mode === "signup" ? "email" : "username"}
            value={email}
            onChange={setEmail}
            error={fieldErrors.email}
            disabled={busy}
          />
          {mode !== "reset" && (
            <Field
              id="pf-gate-password"
              label="Password"
              type="password"
              autoComplete={mode === "signup" ? "new-password" : "current-password"}
              value={password}
              onChange={setPassword}
              error={fieldErrors.password}
              disabled={busy}
              hint={
                mode === "signup"
                  ? `At least ${MIN_PASSWORD} characters`
                  : undefined
              }
            />
          )}

          {mode === "signin" && (
            <button
              type="button"
              className="pf-gate-inline-link"
              onClick={() => switchMode("reset")}
              disabled={busy}
            >
              Forgot password?
            </button>
          )}

          {auth.error && (
            <p className="pf-gate-error" role="alert">
              {auth.error}
            </p>
          )}

          <button
            type="submit"
            className="pf-gate-btn pf-gate-btn-primary"
            disabled={busy}
          >
            {submitting
              ? mode === "signup"
                ? "Creating account…"
                : mode === "reset"
                  ? "Sending…"
                  : "Signing in…"
              : t.cta}
          </button>
        </form>

        <p className="pf-gate-switch">
          {mode === "signin" && (
            <>
              Don't have an account?{" "}
              <button
                type="button"
                className="pf-gate-inline-link"
                onClick={() => switchMode("signup")}
                disabled={busy}
              >
                Sign up
              </button>
            </>
          )}
          {mode === "signup" && (
            <>
              Already have an account?{" "}
              <button
                type="button"
                className="pf-gate-inline-link"
                onClick={() => switchMode("signin")}
                disabled={busy}
              >
                Sign in
              </button>
            </>
          )}
          {mode === "reset" && (
            <button
              type="button"
              className="pf-gate-inline-link"
              onClick={() => switchMode("signin")}
              disabled={busy}
            >
              ← Back to sign in
            </button>
          )}
        </p>

        <p className="pf-gate-fineprint">
          By continuing you agree to enhancement requests being proxied
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

interface FieldProps {
  id: string;
  label: string;
  type: "text" | "email" | "password";
  autoComplete: string;
  value: string;
  onChange: (v: string) => void;
  error?: string;
  hint?: string;
  disabled?: boolean;
}

function Field({
  id,
  label,
  type,
  autoComplete,
  value,
  onChange,
  error,
  hint,
  disabled,
}: FieldProps) {
  return (
    <div className={`pf-gate-field ${error ? "has-error" : ""}`}>
      <label htmlFor={id}>{label}</label>
      <input
        id={id}
        type={type}
        autoComplete={autoComplete}
        value={value}
        onChange={(e) => onChange(e.target.value)}
        disabled={disabled}
        spellCheck={false}
        aria-invalid={error ? "true" : undefined}
        aria-describedby={error ? `${id}-err` : hint ? `${id}-hint` : undefined}
      />
      {error ? (
        <p id={`${id}-err`} className="pf-gate-field-error">
          {error}
        </p>
      ) : hint ? (
        <p id={`${id}-hint`} className="pf-gate-field-hint">
          {hint}
        </p>
      ) : null}
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

