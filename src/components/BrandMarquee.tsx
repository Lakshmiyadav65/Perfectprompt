import "./BrandMarquee.css";

/// Minimal brand footer for the auth surfaces. A tiny lowercase
/// wordmark sits centered at the viewport bottom; below it, a hair-
/// thin line carries a single accent-coloured pulse that traces
/// left to right. The whole thing is sized + opacity'd so it reads
/// as a quiet signature rather than competing with the card above.
export function BrandMarquee() {
  return (
    <div className="pf-marquee" aria-hidden="true">
      <span className="pf-marquee-word">
        perfectprompt<span className="pf-marquee-dot">.</span>
      </span>
      <div className="pf-marquee-line">
        <span className="pf-marquee-pulse" />
      </div>
    </div>
  );
}
