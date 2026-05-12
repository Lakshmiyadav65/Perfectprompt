import "./StatusIndicator.css";

export function StatusIndicator() {
  return (
    <div className="pf-status-pill">
      <div className="pf-spinner" aria-hidden />
      <span className="pf-status-text">Enhancing…</span>
    </div>
  );
}
