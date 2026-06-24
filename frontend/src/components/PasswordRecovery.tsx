import { FormEvent, useState } from "react";
import { useAuth } from "../hooks/useAuth";
import "./SignupGate.css";

interface FieldErrors {
  password?: string;
  confirm?: string;
}

const MIN_PASSWORD = 6;

export function PasswordRecovery() {
  const auth = useAuth();
  const [password, setPassword] = useState("");
  const [confirm, setConfirm] = useState("");
  const [errors, setErrors] = useState<FieldErrors>({});
  const [submitting, setSubmitting] = useState(false);
  const [cancelling, setCancelling] = useState(false);
  const [success, setSuccess] = useState(false);

  function validate(): boolean {
    const errs: FieldErrors = {};
    if (!password) {
      errs.password = "New password is required.";
    } else if (password.length < MIN_PASSWORD) {
      errs.password = `Password must be at least ${MIN_PASSWORD} characters.`;
    }
    if (!confirm) {
      errs.confirm = "Please confirm your new password.";
    } else if (password && confirm !== password) {
      errs.confirm = "Passwords don't match.";
    }
    setErrors(errs);
    return Object.keys(errs).length === 0;
  }

  async function handleSubmit(e: FormEvent) {
    e.preventDefault();
    if (submitting || cancelling) return;
    if (!validate()) return;
    setSubmitting(true);
    try {
      await auth.updatePassword(password);
      setSuccess(true);
    } catch {
      // auth.error is set; stay on the form.
    } finally {
      setSubmitting(false);
    }
  }

  async function handleCancel() {
    if (cancelling || submitting) return;
    setCancelling(true);
    try {
      await auth.cancelRecovery();
    } finally {
      setCancelling(false);
    }
  }

  if (success) {
    return (
      <div className="pf-gate">
        <div className="pf-gate-card">
          <div className="pf-gate-brand">
            <div className="pf-gate-brand-mark" aria-hidden="true" />
            <span className="pf-gate-brand-name">PerfectPrompt</span>
          </div>
          <h1 className="pf-gate-title">Password updated</h1>
          <p className="pf-gate-sub">
            You're all set. Taking you into PerfectPrompt…
          </p>
        </div>
      </div>
    );
  }

  return (
    <div className="pf-gate">
      <div className="pf-gate-card">
        <div className="pf-gate-brand">
          <div className="pf-gate-brand-mark" aria-hidden="true" />
          <span className="pf-gate-brand-name">PerfectPrompt</span>
        </div>

        <h1 className="pf-gate-title">Set a new password</h1>
        <p className="pf-gate-sub">
          Pick something strong — at least {MIN_PASSWORD} characters.
        </p>

        <form className="pf-gate-form" onSubmit={handleSubmit} noValidate>
          <Field
            id="pf-recovery-password"
            label="New password"
            value={password}
            onChange={setPassword}
            error={errors.password}
            disabled={submitting || cancelling}
            autoComplete="new-password"
          />
          <Field
            id="pf-recovery-confirm"
            label="Confirm new password"
            value={confirm}
            onChange={setConfirm}
            error={errors.confirm}
            disabled={submitting || cancelling}
            autoComplete="new-password"
          />

          {auth.error && (
            <p className="pf-gate-error" role="alert">
              {auth.error}
            </p>
          )}

          <button
            type="submit"
            className="pf-gate-btn pf-gate-btn-primary"
            disabled={submitting || cancelling}
          >
            {submitting ? "Updating…" : "Update password"}
          </button>
        </form>

        <p className="pf-gate-switch">
          <button
            type="button"
            className="pf-gate-inline-link"
            onClick={handleCancel}
            disabled={submitting || cancelling}
          >
            {cancelling ? "Cancelling…" : "Cancel and sign out"}
          </button>
        </p>
      </div>
    </div>
  );
}

interface FieldProps {
  id: string;
  label: string;
  value: string;
  onChange: (v: string) => void;
  error?: string;
  disabled?: boolean;
  autoComplete: string;
}

function Field({ id, label, value, onChange, error, disabled, autoComplete }: FieldProps) {
  return (
    <div className={`pf-gate-field ${error ? "has-error" : ""}`}>
      <label htmlFor={id}>{label}</label>
      <input
        id={id}
        type="password"
        autoComplete={autoComplete}
        value={value}
        onChange={(e) => onChange(e.target.value)}
        disabled={disabled}
        spellCheck={false}
        aria-invalid={error ? "true" : undefined}
        aria-describedby={error ? `${id}-err` : undefined}
      />
      {error && (
        <p id={`${id}-err`} className="pf-gate-field-error">
          {error}
        </p>
      )}
    </div>
  );
}
