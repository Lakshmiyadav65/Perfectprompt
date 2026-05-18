import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import "./EnhancementDetail.css";

export interface EnhancementRecord {
  id: string;
  created_at: string;
  rough: string;
  enhanced: string;
  route: string;
  project_id: string | null;
  project_name: string | null;
}

interface Props {
  record: EnhancementRecord;
  onClose: () => void;
}

/// Detail view for a saved enhancement. Opens when the user clicks
/// any row in the dashboard's "Recent enhancements" list and shows
/// the full rough + enhanced text alongside copy + delete actions.
///
/// Renders as a centered modal over the dashboard. Closes on Esc,
/// backdrop click, or the explicit Close button. Deletion is
/// optimistic-via-event: we invoke delete_enhancement, the Rust
/// side broadcasts enhancement-history:deleted, and Home reacts
/// by filtering its list + clearing the selected state (so this
/// component unmounts).
export function EnhancementDetail({ record, onClose }: Props) {
  const cardRef = useRef<HTMLDivElement>(null);
  const [copied, setCopied] = useState(false);
  const [deleting, setDeleting] = useState(false);
  const [confirmDelete, setConfirmDelete] = useState(false);

  useEffect(() => {
    function onKey(e: KeyboardEvent) {
      if (e.key === "Escape") onClose();
    }
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [onClose]);

  function onBackdropClick(e: React.MouseEvent<HTMLDivElement>) {
    if (!cardRef.current?.contains(e.target as Node)) {
      onClose();
    }
  }

  async function handleCopy() {
    try {
      await navigator.clipboard.writeText(record.enhanced);
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1600);
    } catch (e) {
      console.error("[detail] clipboard write failed:", e);
    }
  }

  async function handleDelete() {
    if (deleting) return;
    setDeleting(true);
    try {
      await invoke("delete_enhancement", { id: record.id });
      // Home listens for enhancement-history:deleted and will close
      // this modal by clearing its `selected` state.
    } catch (e) {
      console.error("[detail] delete failed:", e);
      setDeleting(false);
      setConfirmDelete(false);
    }
  }

  const ts = formatFull(record.created_at);
  const routeLabel = labelForRoute(record.route);

  return (
    <div className="pf-detail-backdrop" onMouseDown={onBackdropClick} role="dialog" aria-modal="true">
      <div ref={cardRef} className="pf-detail-card">
        <header className="pf-detail-head">
          <div className="pf-detail-meta">
            <span className="pf-detail-route">{routeLabel}</span>
            {record.project_name && (
              <span className="pf-detail-project" title={`Active project: ${record.project_name}`}>
                {record.project_name}
              </span>
            )}
            <span className="pf-detail-time">{ts}</span>
          </div>
          <button
            type="button"
            className="pf-detail-close"
            onClick={onClose}
            aria-label="Close"
          >
            <CloseIcon />
          </button>
        </header>

        <section className="pf-detail-section">
          <div className="pf-detail-label">Original prompt</div>
          <div className="pf-detail-body pf-detail-body-rough">{record.rough}</div>
        </section>

        <section className="pf-detail-section">
          <div className="pf-detail-label">Enhanced prompt</div>
          <div className="pf-detail-body pf-detail-body-enhanced">{record.enhanced}</div>
        </section>

        <footer className="pf-detail-foot">
          <button
            type="button"
            className="pf-detail-btn pf-detail-btn-primary"
            onClick={handleCopy}
          >
            {copied ? (
              <>
                <CheckIcon /> Copied
              </>
            ) : (
              <>
                <CopyIcon /> Copy enhanced
              </>
            )}
          </button>
          {confirmDelete ? (
            <div className="pf-detail-confirm">
              <span className="pf-detail-confirm-text">Delete this entry?</span>
              <button
                type="button"
                className="pf-detail-link-btn pf-detail-link-btn-danger"
                onClick={handleDelete}
                disabled={deleting}
              >
                {deleting ? "Deleting…" : "Delete"}
              </button>
              <button
                type="button"
                className="pf-detail-link-btn"
                onClick={() => setConfirmDelete(false)}
                disabled={deleting}
              >
                Cancel
              </button>
            </div>
          ) : (
            <button
              type="button"
              className="pf-detail-link-btn pf-detail-link-btn-danger"
              onClick={() => setConfirmDelete(true)}
            >
              Delete
            </button>
          )}
        </footer>
      </div>
    </div>
  );
}

function formatFull(iso: string): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return iso;
  return d.toLocaleString(undefined, {
    weekday: "short",
    month: "short",
    day: "numeric",
    hour: "numeric",
    minute: "2-digit",
  });
}

function labelForRoute(route: string): string {
  switch (route) {
    case "code":
      return "Code";
    case "writing":
      return "Writing";
    case "generic":
      return "General";
    default:
      return route;
  }
}

function CopyIcon() {
  return (
    <svg viewBox="0 0 24 24" width="13" height="13" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
      <rect x="9" y="9" width="11" height="11" rx="2" />
      <path d="M5 15V5a2 2 0 0 1 2-2h10" />
    </svg>
  );
}

function CheckIcon() {
  return (
    <svg viewBox="0 0 24 24" width="13" height="13" fill="none" stroke="currentColor" strokeWidth="2.4" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
      <path d="M20 6 9 17l-5-5" />
    </svg>
  );
}

function CloseIcon() {
  return (
    <svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
      <path d="M6 6l12 12M18 6l-12 12" />
    </svg>
  );
}
