import { ReactNode, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { openUrl } from "@tauri-apps/plugin-opener";
import { Home } from "./Home";
import { Settings } from "./Settings";
import { ProjectManager } from "./ProjectManager";
import { UpdateBanner } from "./UpdateBanner";
import { useEnhancementUsage } from "../hooks/useEnhancementUsage";
import { useAuth } from "../hooks/useAuth";
import { useDisplayName } from "../hooks/useDisplayName";
import { useUpdateCheck } from "../hooks/useUpdateCheck";
import { supabase } from "../lib/supabase";
import "./Shell.css";

/// Mailto fallback when create-subscription can't be reached (dev install
/// without Supabase, function not deployed yet, transient network error).
/// Users get to email support so payment can still happen by hand.
const SUBSCRIPTION_FALLBACK =
  "mailto:enviguide.official@gmail.com?subject=PerfectPrompt%20Pro%20%E2%80%94%20%E2%82%B999%2Fmonth";

/// Ask create-subscription for a fresh Razorpay subscription checkout
/// URL. If the user already has an active subscription, the function
/// returns the existing portal URL (so "Upgrade" doubles as "Manage").
/// "renews Apr 14" style line for the pro_active hint, using the user's
/// locale for the date format. Returns a friendly fallback string when
/// currentPeriodEnd is null — happens briefly between subscription.created
/// and the first subscription.charged webhook.
function renewLine(periodEnd: string | null): string {
  if (!periodEnd) return "Pro · subscription active";
  const date = new Date(periodEnd);
  if (Number.isNaN(date.getTime())) return "Pro · subscription active";
  const fmt = date.toLocaleDateString(undefined, { month: "short", day: "numeric" });
  return `Renews ${fmt}`;
}

/// "resets in 4h 12m" countdown for free users at the cap. Returns
/// empty string if resetsAt is malformed or already past (the next
/// /enhance call will refresh it).
function resetsInLine(resetsAt: string | null): string {
  if (!resetsAt) return "";
  const target = new Date(resetsAt).getTime();
  if (Number.isNaN(target)) return "";
  const diffMs = target - Date.now();
  if (diffMs <= 0) return "";
  const totalMin = Math.ceil(diffMs / 60_000);
  if (totalMin < 60) return `Resets in ${totalMin}m`;
  const h = Math.floor(totalMin / 60);
  const m = totalMin % 60;
  return m === 0 ? `Resets in ${h}h` : `Resets in ${h}h ${m}m`;
}

/// Self-heal hook for missed Razorpay webhooks. Asks the verify-subscription
/// edge function to fetch the user's current subscription state directly
/// from Razorpay's API and reconcile our profile row. Cheap one-shot
/// call (~300ms) that we run before every Upgrade-button click AND
/// periodically after the Razorpay tab opens — if the customer paid but
/// a webhook got lost, this catches it without any manual SQL.
///
/// Returns true if the user is now on a paid plan (pro / unlimited)
/// after the sync. Dispatches a `pf-quota-refresh` event on success so
/// the sidebar refreshes immediately — without that, an at-cap free
/// user who just paid would stay stuck on "Daily limit reached" until
/// their next /enhance call.
async function verifyAndSync(): Promise<boolean> {
  try {
    const { data, error } = await supabase.functions.invoke<{
      plan_tier: string;
      subscription_status: string | null;
      synced: boolean;
    }>("verify-subscription", { method: "POST" });
    if (error) {
      // Function might not be deployed yet, or transient network blip.
      // Best-effort — fall through to the normal create-subscription path.
      console.warn("[shell] verify-subscription unavailable:", error);
      return false;
    }
    const isPaid =
      data?.plan_tier === "pro" || data?.plan_tier === "unlimited";
    if (isPaid) {
      // Tell useEnhancementUsage to re-read profile + daily_usage. The
      // hook converts that into the new sidebar state ("Pro · Renews ...").
      window.dispatchEvent(new CustomEvent("pf-quota-refresh"));
    }
    return isPaid;
  } catch (e) {
    console.warn("[shell] verify-subscription threw:", e);
    return false;
  }
}

/// Ask create-subscription for a fresh Razorpay subscription checkout
/// URL. If the user already has an in-flight subscription, the function
/// returns the existing portal URL (so "Upgrade" doubles as "Manage").
/// After the Razorpay checkout tab opens, poll verify-subscription
/// every 5 seconds for up to 3 minutes. Catches the case where the
/// customer pays, Razorpay's webhook never reaches us, and the
/// customer returns to the app expecting Pro access. Each poll is a
/// single Razorpay GET + a conditional DB update; cheap.
///
/// Stops on:
///   - first poll where the user is detected as paid (success)
///   - 3-minute timeout (give up; user can click Upgrade again to retry)
///   - explicit cancel (e.g., AbortController triggered by component unmount)
async function pollForPaidStatus(signal: AbortSignal): Promise<void> {
  const POLL_INTERVAL_MS = 5_000;
  const POLL_DEADLINE_MS = 3 * 60 * 1000;
  const startedAt = Date.now();

  while (Date.now() - startedAt < POLL_DEADLINE_MS) {
    if (signal.aborted) return;
    await new Promise((r) => setTimeout(r, POLL_INTERVAL_MS));
    if (signal.aborted) return;
    try {
      const isPaid = await verifyAndSync();
      if (isPaid) {
        // verifyAndSync already dispatched pf-quota-refresh; done.
        return;
      }
    } catch (e) {
      // Don't break the loop on a single transient failure — Razorpay
      // or Supabase can blip; the next tick will retry.
      console.warn("[shell] poll iteration failed:", e);
    }
  }
}

async function requestSubscriptionUrl(): Promise<string> {
  try {
    const { data, error } = await supabase.functions.invoke<{
      url: string;
      existing?: boolean;
    }>("create-subscription", { method: "POST" });
    if (error) {
      console.error("[shell] create-subscription errored:", error);
      return SUBSCRIPTION_FALLBACK;
    }
    const url = data?.url?.trim();
    if (!url) {
      console.error("[shell] create-subscription returned no url");
      return SUBSCRIPTION_FALLBACK;
    }
    return url;
  } catch (e) {
    console.error("[shell] create-subscription threw:", e);
    return SUBSCRIPTION_FALLBACK;
  }
}

type Route = "home" | "projects" | "settings";
export type FocusTarget = "api-key" | null;

interface ApiKeyStatus {
  from_env: boolean;
  from_settings: boolean;
}

interface Project {
  id: string;
  name: string;
  description: string;
  links: string[];
  path: string | null;
  created_at: string;
  updated_at: string;
}

interface ProjectStore {
  active_project_id: string | null;
  projects: Project[];
}

const NAV: { route: Route; label: string; icon: ReactNode }[] = [
  {
    route: "home",
    label: "Home",
    icon: (
      <svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round">
        <path d="M3 12 12 3l9 9" />
        <path d="M5 10v10h14V10" />
      </svg>
    ),
  },
  {
    route: "projects",
    label: "Projects",
    icon: (
      <svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round">
        <path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v9a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V7z" />
      </svg>
    ),
  },
  {
    route: "settings",
    label: "Settings",
    icon: (
      <svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round">
        <circle cx="12" cy="12" r="3" />
        <path d="M19.4 15a1.7 1.7 0 0 0 .3 1.8l.1.1a2 2 0 1 1-2.8 2.8l-.1-.1a1.7 1.7 0 0 0-1.8-.3 1.7 1.7 0 0 0-1 1.5V21a2 2 0 1 1-4 0v-.1a1.7 1.7 0 0 0-1.1-1.5 1.7 1.7 0 0 0-1.8.3l-.1.1a2 2 0 1 1-2.8-2.8l.1-.1a1.7 1.7 0 0 0 .3-1.8 1.7 1.7 0 0 0-1.5-1H3a2 2 0 1 1 0-4h.1a1.7 1.7 0 0 0 1.5-1 1.7 1.7 0 0 0-.3-1.8l-.1-.1a2 2 0 1 1 2.8-2.8l.1.1a1.7 1.7 0 0 0 1.8.3h.1a1.7 1.7 0 0 0 1-1.5V3a2 2 0 1 1 4 0v.1a1.7 1.7 0 0 0 1 1.5 1.7 1.7 0 0 0 1.8-.3l.1-.1a2 2 0 1 1 2.8 2.8l-.1.1a1.7 1.7 0 0 0-.3 1.8v.1a1.7 1.7 0 0 0 1.5 1H21a2 2 0 1 1 0 4h-.1a1.7 1.7 0 0 0-1.5 1z" />
      </svg>
    ),
  },
];

// Reads (and clears) a pending post-auth focus breadcrumb dropped by
// PostAuthSetup. Returns the initial route + focus target Shell should
// boot into. Falls back to whatever the parent passed (typically
// "home") when nothing is pending.
function consumePostAuthBreadcrumb(fallback: Route): {
  route: Route;
  focus: FocusTarget;
} {
  try {
    const pending = sessionStorage.getItem("pf:post-auth-focus");
    if (pending === "api-key") {
      sessionStorage.removeItem("pf:post-auth-focus");
      return { route: "settings", focus: "api-key" };
    }
  } catch {
    // sessionStorage can throw in private-mode embeds / sandboxes —
    // not worth crashing the shell over.
  }
  return { route: fallback, focus: null };
}

export function Shell({ initial }: { initial: Route }) {
  // Boot lazily from the breadcrumb so we never flicker through Home
  // on the way to Settings — the user explicitly asked for the API
  // setup section, hand it to them on the first render. Lazy init so
  // the sessionStorage read + clear runs exactly once.
  const [boot] = useState(() => consumePostAuthBreadcrumb(initial));
  const [route, setRoute] = useState<Route>(boot.route);
  // When a deep-link CTA (Home setup card, banner, status tile) sends
  // the user to Settings, we pass an explicit focus target so Settings
  // can scroll + focus the right section instead of dumping them at
  // the page top.
  const [focusTarget, setFocusTarget] = useState<FocusTarget>(boot.focus);
  const [hotkey, setHotkey] = useState<string>("Ctrl+Alt+E");
  const [keyStatus, setKeyStatus] = useState<ApiKeyStatus | null>(null);
  const [enabled, setEnabled] = useState<boolean>(true);
  const [toggling, setToggling] = useState(false);
  const [store, setStore] = useState<ProjectStore>({ active_project_id: null, projects: [] });
  /// AbortController for the post-payment polling loop. Tracked at the
  /// Shell level (not inside the click handler) so the polling can be
  /// cancelled when the user navigates away or signs out before the
  /// 3-minute deadline elapses.
  const pollAbortRef = useRef<AbortController | null>(null);
  useEffect(() => {
    return () => {
      pollAbortRef.current?.abort();
    };
  }, []);
  const usage = useEnhancementUsage();
  // Background update check — runs once on mount + every 6h. Renders
  // a bottom-right toast when a newer GitHub release exists. Failures
  // are silent; the manual button in Settings → Updates still works
  // for users who want explicit control.
  const update = useUpdateCheck();
  // Progress bar only renders for metered (free / lapsed) states where
  // both used + limit are real numbers. Pro / unlimited render without
  // a bar.
  const usagePct = usage.limit && usage.limit > 0
    ? Math.min(100, Math.round((usage.used / usage.limit) * 100))
    : 0;
  const auth = useAuth();
  const displayName = useDisplayName(auth.user);
  const [profileMenuOpen, setProfileMenuOpen] = useState(false);
  const profileChipRef = useRef<HTMLButtonElement>(null);
  const profileMenuRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    invoke<string>("get_hotkey").then(setHotkey).catch(() => {});
    invoke<ApiKeyStatus>("api_key_status").then(setKeyStatus).catch(() => {});
    invoke<boolean>("get_hotkey_enabled").then(setEnabled).catch(() => {});
    invoke<ProjectStore>("list_projects").then(setStore).catch(() => {});
  }, [route]);

  // Poll for external state flips (e.g. capsule X persists Paused).
  useEffect(() => {
    const id = window.setInterval(() => {
      if (toggling) return;
      invoke<boolean>("get_hotkey_enabled").then(setEnabled).catch(() => {});
      invoke<ProjectStore>("list_projects").then(setStore).catch(() => {});
    }, 1500);
    return () => window.clearInterval(id);
  }, [toggling]);

  // Close the profile menu when the user clicks anywhere outside it
  // or hits Escape. Listener only attaches while the menu is open so
  // we don't pay the cost on every render.
  useEffect(() => {
    if (!profileMenuOpen) return;
    function onPointer(e: MouseEvent) {
      const target = e.target as Node;
      if (
        profileMenuRef.current?.contains(target) ||
        profileChipRef.current?.contains(target)
      ) {
        return;
      }
      setProfileMenuOpen(false);
    }
    function onKey(e: KeyboardEvent) {
      if (e.key === "Escape") setProfileMenuOpen(false);
    }
    document.addEventListener("mousedown", onPointer);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onPointer);
      document.removeEventListener("keydown", onKey);
    };
  }, [profileMenuOpen]);

  async function handleSignOut() {
    setProfileMenuOpen(false);
    try {
      await auth.signOut();
    } catch (e) {
      console.error("[shell] sign out failed:", e);
    }
  }

  async function handleToggle() {
    if (toggling) return;
    setToggling(true);
    const prev = enabled;
    const next = !enabled;
    setEnabled(next);
    try {
      await invoke("set_hotkey_enabled", { enabled: next });
      try {
        await invoke(next ? "show_command_bar" : "hide_command_bar");
      } catch (e) {
        console.error("command bar visibility toggle failed", e);
      }
    } catch (e) {
      console.error("toggle failed", e);
      setEnabled(prev);
    } finally {
      setToggling(false);
    }
  }

  // Tray menu items navigate by setting `window.location.hash`. Listen for
  // those changes so the sidebar stays in sync without a full reload.
  useEffect(() => {
    function handler() {
      const hash = window.location.hash.replace(/^#\//, "").trim();
      if (hash === "projects" || hash === "settings" || hash === "home") {
        setRoute(hash);
      }
    }
    window.addEventListener("hashchange", handler);
    return () => window.removeEventListener("hashchange", handler);
  }, []);

  useEffect(() => {
    const desired = `#/${route}`;
    if (window.location.hash !== desired) {
      history.replaceState(null, "", desired);
    }
  }, [route]);

  // Cross-screen navigation with an optional focus target. Sidebar
  // and tray callers pass no target (just route changes); CTA
  // surfaces like the Home setup card pass "api-key" so Settings
  // scrolls + focuses the Groq API Key section on arrival.
  function navigate(next: Route, focus: FocusTarget = null) {
    if (focus) setFocusTarget(focus);
    setRoute(next);
  }

  const ready = !!(keyStatus?.from_env || keyStatus?.from_settings);
  // Tauri returns the canonical "CommandOrControl" / "Option" / "Super"
  // strings — too verbose for the 232px sidebar. Display the short,
  // OS-friendly forms.
  const hotkeyParts = hotkey.split("+").map(prettyKey);

  // Mocked "Top 2%" badge gate: shows only when the user is fully set
  // up (API key configured AND at least one project attached). The
  // copy ("Top 2%") is decorative until we ship real telemetry — the
  // intent is to let power users feel the app recognise them. Wire
  // to a real percentile once enhancement history lives on disk.
  const isPowerUser = ready && store.projects.length > 0;
  // Top 2 most-recently-updated projects for the sidebar "Recent" list.
  const recentProjects = [...store.projects]
    .sort((a, b) => (a.updated_at < b.updated_at ? 1 : -1))
    .slice(0, 3);

  return (
    <div className="pf-shell">
      <aside className="pf-sidebar">
        <div className="pf-brand">
          <BrandMark />
          <div className="pf-brand-name">PerfectPrompt</div>
        </div>

        <nav className="pf-nav" aria-label="Primary">
          {NAV.map((item) => (
            <button
              key={item.route}
              type="button"
              className={`pf-nav-item ${route === item.route ? "active" : ""}`}
              onClick={() => setRoute(item.route)}
            >
              <span className="pf-nav-icon">{item.icon}</span>
              <span>{item.label}</span>
              {item.route === "projects" && (
                <span className="pf-beta-badge" aria-label="Beta feature">Beta</span>
              )}
            </button>
          ))}

          {recentProjects.length > 0 && (
            <>
              <div className="pf-nav-section">Recent</div>
              {recentProjects.map((p) => (
                <button
                  key={p.id}
                  type="button"
                  className="pf-nav-item pf-nav-recent"
                  onClick={() => setRoute("projects")}
                >
                  <span
                    className={`pf-recent-dot ${
                      p.id === store.active_project_id ? "active" : ""
                    }`}
                  />
                  <span className="pf-recent-name">{p.name}</span>
                </button>
              ))}
            </>
          )}
        </nav>

        <div className="pf-sidebar-bottom">
          {/* Usage card. Source-of-truth is consume_daily_quota +
              the profile row; the hook merges them. States rendered:
                - free_under_limit  → "Free · 3 / 10 today"
                - special_access    → "Special access · 3 / 15 today"
                - free_at_limit     → "Daily limit reached" + Upgrade CTA
                - pro_active        → "Pro · renews <date>"
                - pro_lapsed        → "Subscription lapsed" + Resubscribe CTA
                - unlimited         → "Unlimited access" (admin-granted, no caps)
              BYOK / signed-out users (`status === 'byok'`) hide the
              entire card — they pay Groq directly, no paywall applies. */}
          {usage.status !== "byok" && (
            <div className={`pf-usage-card ${usage.limitReached ? "limit-reached" : ""}`}>
              <div className="pf-usage-head">
                <span className="pf-usage-label">
                  {usage.status === "unlimited"
                    ? "Unlimited"
                    : usage.status === "pro_active"
                    ? "Pro"
                    : usage.status === "pro_lapsed"
                    ? "Pro · lapsed"
                    : usage.status === "special_access"
                    ? "Special access"
                    : "Free"}
                </span>
                {(usage.status === "free_under_limit"
                  || usage.status === "free_at_limit"
                  || usage.status === "special_access"
                  || usage.status === "pro_lapsed") && usage.limit !== null && (
                  <span className="pf-usage-count">
                    <strong>{usage.used}</strong>
                    <span className="pf-usage-sep">/</span>
                    {usage.limit}
                  </span>
                )}
              </div>
              {(usage.status === "free_under_limit"
                || usage.status === "free_at_limit"
                || usage.status === "special_access"
                || usage.status === "pro_lapsed") && usage.limit !== null && (
                <div className="pf-usage-bar" aria-hidden="true">
                  <div
                    className="pf-usage-bar-fill"
                    style={{ width: `${usagePct}%` }}
                  />
                </div>
              )}
              <div className="pf-usage-hint">
                {usage.status === "unlimited"
                  ? "Unlimited enhancements"
                  : usage.status === "pro_active"
                  ? renewLine(usage.currentPeriodEnd)
                  : usage.status === "pro_lapsed"
                  ? "Subscription expired — resubscribe to keep pro."
                  : usage.status === "free_at_limit"
                  ? `Daily limit reached. ${resetsInLine(usage.resetsAt)}`
                  : usage.status === "special_access"
                  ? "Admin-granted daily bonus active"
                  : "Free enhancements per day"}
              </div>
              {/* Upgrade CTA for anyone who can subscribe.
                  - free_at_limit / pro_lapsed: primary upgrade trigger
                  - pro_active: small "manage subscription" affordance
                  - unlimited: nothing (they're grandfathered) */}
              {(usage.limitReached || usage.status === "pro_active") && (
                <button
                  type="button"
                  className="pf-usage-upgrade"
                  onClick={async () => {
                    // Self-heal first: if the user paid earlier but a
                    // webhook got lost, verify-subscription syncs the
                    // profile from Razorpay's authoritative state.
                    // Skip the checkout flow if they're already paid.
                    // verifyAndSync dispatches pf-quota-refresh on
                    // success so the sidebar updates immediately.
                    const isAlreadyPaid = await verifyAndSync();
                    if (isAlreadyPaid && usage.status !== "pro_active") {
                      return;
                    }
                    // For pro users, the function returns their existing
                    // subscription portal URL (so this button doubles
                    // as "Manage"). For free / lapsed users it creates
                    // a fresh subscription and returns the checkout URL.
                    const url = await requestSubscriptionUrl();
                    openUrl(url).catch((e) =>
                      console.error("[shell] subscription link open failed", e),
                    );
                    // After the Razorpay tab opens, start polling for
                    // payment completion. Catches the case where the
                    // customer pays but Razorpay's subscription.activated
                    // webhook never reaches our function. Cancel any
                    // previous poll first — clicking Upgrade twice
                    // shouldn't run two concurrent loops.
                    if (usage.status !== "pro_active") {
                      pollAbortRef.current?.abort();
                      const controller = new AbortController();
                      pollAbortRef.current = controller;
                      void pollForPaidStatus(controller.signal);
                    }
                  }}
                >
                  {usage.status === "pro_active"
                    ? "Manage subscription"
                    : usage.status === "pro_lapsed"
                    ? "Resubscribe for ₹99/mo"
                    : "Upgrade for ₹99/mo"}
                </button>
              )}
            </div>
          )}

          <div className={`pf-listening-card ${enabled && ready ? "live" : ""}`}>
            <div className="pf-listening-row">
              <span className="pf-listening-dot" />
              <span className="pf-listening-label">
                {ready
                  ? enabled
                    ? "Prompt Mode On"
                    : "Prompt Mode Off"
                  : "Setup"}
              </span>
              <button
                type="button"
                className={`pf-toggle-mini ${enabled ? "on" : ""}`}
                onClick={() => void handleToggle()}
                disabled={toggling || !ready}
                aria-label={enabled ? "Pause" : "Activate"}
                title={enabled ? "Pause PerfectPrompt" : "Activate PerfectPrompt"}
              >
                <span className="pf-toggle-mini-dot" />
              </button>
            </div>
            <div className="pf-listening-hint">Trigger from anywhere</div>
            <div className="pf-listening-keys">
              {hotkeyParts.map((part, i) => (
                <span key={i} className="pf-keypair">
                  <kbd className="pf-kbd">{part}</kbd>
                  {i < hotkeyParts.length - 1 && (
                    <span className="pf-kbd-plus">+</span>
                  )}
                </span>
              ))}
            </div>
          </div>

          {/* Profile chip — Discord/Slack style anchor for user
              identity. Clicking opens a small menu with Settings +
              Sign out so the sign-out path doesn't require digging
              into Settings → Account. Name resolves via
              useDisplayName: override > Google name > email
              local-part > "You". */}
          <div className="pf-profile-wrapper">
            {profileMenuOpen && (
              <div
                ref={profileMenuRef}
                className="pf-profile-menu"
                role="menu"
                aria-label="Account menu"
              >
                <div className="pf-profile-menu-header">
                  <div className="pf-profile-menu-name">{displayName.name}</div>
                  {auth.user?.email && (
                    <div className="pf-profile-menu-email">{auth.user.email}</div>
                  )}
                </div>
                <button
                  type="button"
                  role="menuitem"
                  className="pf-profile-menu-item"
                  onClick={() => {
                    setProfileMenuOpen(false);
                    setRoute("settings");
                  }}
                >
                  Open Settings
                </button>
                <button
                  type="button"
                  role="menuitem"
                  className="pf-profile-menu-item pf-profile-menu-item-danger"
                  onClick={() => void handleSignOut()}
                  disabled={!auth.user}
                >
                  Sign out
                </button>
              </div>
            )}
            <button
              ref={profileChipRef}
              type="button"
              className={`pf-profile-chip ${profileMenuOpen ? "open" : ""}`}
              onClick={() => setProfileMenuOpen((open) => !open)}
              aria-haspopup="menu"
              aria-expanded={profileMenuOpen}
              aria-label={`${displayName.name} — account menu`}
            >
              <span className="pf-profile-avatar" aria-hidden="true">
                {displayName.name.charAt(0).toUpperCase()}
              </span>
              <span className="pf-profile-text">
                <span className="pf-profile-name-row">
                  <span className="pf-profile-name">{displayName.name}</span>
                  {isPowerUser && (
                    <span
                      className="pf-profile-badge"
                      title="Top 2% — based on your local activity"
                      aria-label="Top two percent"
                    >
                      <svg
                        viewBox="0 0 24 24"
                        width="9"
                        height="9"
                        fill="currentColor"
                        aria-hidden="true"
                      >
                        <path d="M12 2l2.6 6.3 6.8.6-5.2 4.5 1.6 6.6L12 16.8 6.2 20l1.6-6.6L2.6 8.9l6.8-.6L12 2z" />
                      </svg>
                      Top 2%
                    </span>
                  )}
                </span>
              </span>
              <svg
                className="pf-profile-chev"
                viewBox="0 0 24 24"
                width="14"
                height="14"
                fill="none"
                stroke="currentColor"
                strokeWidth="1.6"
                strokeLinecap="round"
                strokeLinejoin="round"
                aria-hidden="true"
              >
                <path d="M9 6l6 6-6 6" />
              </svg>
            </button>
          </div>
        </div>
      </aside>

      <main className="pf-main">
        {route === "home" && <Home onNavigate={navigate} />}
        {route === "projects" && <ProjectManager />}
        {route === "settings" && (
          <Settings
            focusTarget={focusTarget}
            onFocusHandled={() => setFocusTarget(null)}
          />
        )}
      </main>

      {/* Bottom-right toast surfaced by useUpdateCheck whenever a
          newer GitHub release exists than the locally-installed
          version. Persists across route changes inside the main
          window; dismissed banners stay dismissed for that exact
          version across launches via localStorage. */}
      {update.showBanner && update.updateInfo && (
        <UpdateBanner
          version={update.updateInfo.latest_version}
          releaseUrl={update.updateInfo.release_url}
          onDismiss={update.dismiss}
        />
      )}
    </div>
  );
}

/// Conic-gradient brand mark — matches the mockup. No fill image needed,
/// renders crisply at 28px.
function BrandMark() {
  return (
    <div className="pf-brand-mark" aria-hidden="true" />
  );
}

/// Translates Tauri's canonical modifier names to the short labels users
/// expect on Windows. macOS users would see ⌘/⌥ — we'll branch when we
/// ship that platform.
function prettyKey(part: string): string {
  switch (part.trim()) {
    case "CommandOrControl":
    case "Control":
      return "Ctrl";
    case "Option":
    case "Alt":
      return "Alt";
    case "Super":
    case "Command":
      return "Win";
    default:
      return part;
  }
}
