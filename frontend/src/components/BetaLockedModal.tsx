import { useEffect } from "react";

import type { ProjectWaitlist } from "../hooks/useProjectWaitlist";
import "./BetaLockedModal.css";

/// "Adding projects is coming soon" popup. Project creation is gated
/// during the beta, so any CTA that would start the add-project flow
/// (the Projects screen's "+ Add Project", Home's "Add project
/// context") opens this instead. It explains why and offers a
/// one-click waitlist signup.
///
/// Self-contained — own CSS, no dependency on ProjectManager's styles —
/// so it can be rendered from anywhere. The waitlist hook is owned by
/// the caller and passed in, so a screen that also shows the waitlist
/// banner keeps both in sync from one source of truth.
export function BetaLockedModal({
  waitlist,
  onClose,
}: {
  waitlist: ProjectWaitlist;
  onClose: () => void;
}) {
  const { status, error, join } = waitlist;
  const joined = status === "joined";
  const pending = status === "loading" || status === "joining";

  // Esc-to-close.
  useEffect(() => {
    function onKey(e: KeyboardEvent) {
      if (e.key === "Escape") onClose();
    }
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [onClose]);

  return (
    <div className="bwl-overlay" onClick={onClose}>
      <div
        className="bwl-modal"
        onClick={(e) => e.stopPropagation()}
        role="dialog"
        aria-modal="true"
        aria-label="Projects is in beta"
      >
        <div className={`bwl-icon ${joined ? "is-joined" : ""}`} aria-hidden="true">
          {joined ? "✓" : "🔒"}
        </div>

        <h3 className="bwl-title">
          {joined ? "You're on the waitlist" : "Adding projects is coming soon"}
        </h3>

        <p className="bwl-text">
          {joined
            ? "Thanks for signing up — we'll email you the moment Projects opens up and you can start adding your own."
            : "Projects is still in beta, so creating projects isn't available just yet. We're putting the finishing touches on it — join the waitlist and we'll let you know the moment it's ready."}
        </p>

        {error && (
          <p className="bwl-error" role="alert">
            {error}
          </p>
        )}

        <div className="bwl-actions">
          <button type="button" className="bwl-btn-secondary" onClick={onClose}>
            {joined ? "Close" : "Maybe later"}
          </button>
          {!joined && (
            <button
              type="button"
              className="bwl-btn-primary"
              onClick={() => void join()}
              disabled={pending}
            >
              {status === "joining" ? "Joining…" : "Join the waitlist"}
            </button>
          )}
        </div>
      </div>
    </div>
  );
}
