import "./BrandMarquee.css";

/// Large decorative wordmark that scrolls horizontally along the
/// bottom of the auth surfaces (signin / signup / reset, and the
/// post-auth setup interstitial). Pure atmosphere — aria-hidden so
/// screen readers skip it. The text is intentionally tall enough to
/// clip below the visible area for the "infinite wordmark" feel.
export function BrandMarquee() {
  return (
    <div className="pf-marquee" aria-hidden="true">
      <div className="pf-marquee-track">
        {/* Items rendered twice so translateX(-50%) loops seamlessly
            — the second half slides into the position the first half
            started in, with no visible reset. */}
        {Array.from({ length: 2 }).flatMap((_, half) =>
          Array.from({ length: 5 }).map((__, i) => (
            <span key={`${half}-${i}`} className="pf-marquee-item">
              PerfectPrompt<span className="pf-marquee-dot">.</span>
            </span>
          )),
        )}
      </div>
    </div>
  );
}
