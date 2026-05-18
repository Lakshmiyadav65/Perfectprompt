import { useEffect, useState } from "react";
import "./AuthFlowPreview.css";

interface Example {
  rough: string;
  enhanced: string;
}

const EXAMPLES: Example[] = [
  {
    rough: "email my manager about pto next thu/fri",
    enhanced:
      "Write a short, warm-but-professional email to my manager asking for PTO next Thursday and Friday. Mention I'll document open tasks before I leave and offer to handle anything urgent over Slack.",
  },
  {
    rough: "linkedin post launching v1",
    enhanced:
      "Write a LinkedIn post announcing the v1 launch of PerfectPrompt. Lead with the problem rough prompts cause, share one specific early-user moment, and end with a soft CTA for feedback.",
  },
  {
    rough: "explain bearer tokens to a junior dev",
    enhanced:
      "Write a 3-paragraph explanation of how bearer-token authentication works, aimed at a junior developer who knows JavaScript. Use a concrete login → API call walkthrough and avoid jargon.",
  },
  {
    rough: "fix the dashboard load time",
    enhanced:
      "Refactor the analytics dashboard's initial-load path to reduce TTI. Identify the slowest two queries, hoist them above the layout-blocking effect, and add a skeleton state for the chart pane.",
  },
];

const CYCLE_MS = 7800;

/// Decorative "live flow" preview that sits below the auth card on
/// the signup/signin and PostAuthSetup screens. Cycles through
/// rough → enhanced example pairs so users see what PerfectPrompt
/// actually does while they're authenticating. Pure decoration —
/// not interactive, aria-hidden so screen readers skip it.
export function AuthFlowPreview() {
  const [index, setIndex] = useState(0);

  useEffect(() => {
    const id = window.setInterval(() => {
      setIndex((i) => (i + 1) % EXAMPLES.length);
    }, CYCLE_MS);
    return () => window.clearInterval(id);
  }, []);

  const example = EXAMPLES[index];

  return (
    <div className="pf-flow-preview" aria-hidden="true">
      <div className="pf-flow-label">
        <span className="pf-flow-dot" />
        <span>live flow · perfectprompt</span>
      </div>

      {/* The inner block is keyed on `index` so React remounts it
          every cycle — that restarts the CSS keyframes from t=0
          without manual animation reset gymnastics. */}
      <div className="pf-flow-card" key={index}>
        <div className="pf-flow-rough">
          <span className="pf-flow-prefix">rough</span>
          <span className="pf-flow-text pf-flow-text-rough">{example.rough}</span>
        </div>
        <div className="pf-flow-divider" aria-hidden="true">
          <span /><span /><span />
        </div>
        <div className="pf-flow-enhanced">
          <span className="pf-flow-prefix">enhanced</span>
          <span className="pf-flow-text pf-flow-text-enhanced">{example.enhanced}</span>
        </div>
      </div>
    </div>
  );
}
