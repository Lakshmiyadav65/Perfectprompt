import { useEffect } from "react";
import "./StatusIndicator.css";

export function StatusIndicator() {
  // Shell.css sets `html, body, #root { background: var(--pf-bg-deep) }`
  // unconditionally because it's bundled by every App import. Without this
  // override, the status window paints a dark rectangle behind the pill
  // instead of being truly transparent. Inline-style on documentElement
  // outranks every stylesheet, so we don't depend on bundler CSS order.
  useEffect(() => {
    document.body.classList.add("status-route");
    const prevHtmlBg = document.documentElement.style.background;
    document.documentElement.style.background = "transparent";
    return () => {
      document.body.classList.remove("status-route");
      document.documentElement.style.background = prevHtmlBg;
    };
  }, []);

  return (
    <div className="pf-status-pill">
      <div className="pf-spinner" aria-hidden />
      <span className="pf-status-text">Enhancing…</span>
    </div>
  );
}
