import { useEffect, useState } from "react";
import "./BrandMarquee.css";

/// Short taglines that cycle through the brand footer. Each is
/// revealed left-to-right via a clip-path animation, held, then
/// faded out before the next one takes its place.
const TAGLINES = [
  "refine prompts in place",
  "ctrl + alt + e, from anywhere",
  "rough → ready in about a second",
  "powered by groq",
];

/// Length of one full reveal → hold → fade cycle, in ms.
/// Matches the keyframe percentages in BrandMarquee.css —
/// keep the two in sync or the React-side rotation will drift
/// out of phase with the CSS animation.
const CYCLE_MS = 5600;

/// Minimal brand footer for the auth surfaces. Three visible
/// elements:
///   1. tiny lowercase "perfectprompt" wordmark (static)
///   2. accent-colored period (static)
///   3. cycling tagline that types in, holds, fades out, then
///      gets replaced with the next entry via a React key bump
///      that restarts the CSS animation from t=0.
/// Pure decoration — aria-hidden so AT skips it entirely.
export function BrandMarquee() {
  const [taglineIndex, setTaglineIndex] = useState(0);

  useEffect(() => {
    const id = window.setInterval(() => {
      setTaglineIndex((i) => (i + 1) % TAGLINES.length);
    }, CYCLE_MS);
    return () => window.clearInterval(id);
  }, []);

  return (
    <div className="pf-marquee" aria-hidden="true">
      <span className="pf-marquee-row">
        <span className="pf-marquee-word">
          perfectprompt<span className="pf-marquee-dot">.</span>
        </span>
        {/* Key on the index so each cycle remounts the span and
            restarts the type-in keyframes from scratch — no manual
            animation reset needed. */}
        <span className="pf-marquee-tagline" key={taglineIndex}>
          {TAGLINES[taglineIndex]}
        </span>
      </span>
      <span className="pf-marquee-cursor" />
    </div>
  );
}
