import { useEffect, useMemo, useState } from "react";
import { listen } from "@tauri-apps/api/event";

/// Lifetime free-trial tracker for the hosted enhancement tier.
///
/// Replaces the previous daily-quota model. Every signed-in user starts
/// with 10 free enhancements; admin grants (extra_granted on the profile
/// row) bump that ceiling per-user; paid users (plan_tier in 'pro' /
/// 'unlimited') bypass the cap entirely.
///
/// Server is the source of truth — public.consume_lifetime_quota in
/// Postgres is the only place that decides "allowed / denied". The
/// hosted /enhance edge function emits the resulting quota object as
/// `hosted:quota` after every call (success or 429), so this hook just
/// snapshots whatever the server told us last.
///
/// BYOK / signed-out users have no quota at all — they pay Groq directly
/// out-of-pocket, so the paywall doesn't apply. For them we render
/// `source: "byok"` and the sidebar suppresses the usage card.

const FREE_LIFETIME_LIMIT = 10;

type PlanTier = "free_hosted" | "pro" | "unlimited" | string;

interface HostedQuota {
  used: number;
  limit: number;
  remaining: number;
  plan_tier: PlanTier;
  /// Always null under the lifetime model (no daily reset). Kept on the
  /// type so the existing emit on the Rust side serialises cleanly.
  resets_at?: string | null;
}

export type UsageStatus =
  /// Hosted free user, lifetime_used < limit. Can enhance.
  | "free_under_limit"
  /// Hosted free user, lifetime_used >= limit (no admin grant or all
  /// admin-granted units consumed). Pipeline blocked, upgrade CTA shown.
  | "free_at_limit"
  /// Hosted free user with extra_granted > 0 (admin manually unlocked
  /// extra usage). Same enhance-or-block rule as free_under_limit but
  /// the UI surfaces "Special access".
  | "special_access"
  /// plan_tier in {'pro', 'unlimited'} — paid through Razorpay or
  /// granted permanent access by admin. Bypasses the cap.
  | "paid"
  /// Not signed in / no Supabase backend wired. BYOK pays Groq directly;
  /// no app-level paywall applies.
  | "byok";

export interface UsageState {
  status: UsageStatus;
  used: number;
  limit: number;
  remaining: number;
  /// True iff a) the user is on a hosted plan with a cap AND b) they've
  /// hit it. The pipeline / global hotkey is allowed to short-circuit
  /// when this is true so the LLM call is never made for blocked users.
  limitReached: boolean;
  /// True when the user is in `special_access` — i.e. on the free plan
  /// but with extra_granted > 0. Drives the "Special access" badge in
  /// the sidebar card.
  hasAdminGrant: boolean;
  planTier: PlanTier | null;
}

function deriveStatus(q: HostedQuota | null): UsageState {
  if (!q) {
    return {
      status: "byok",
      used: 0,
      limit: FREE_LIFETIME_LIMIT,
      remaining: FREE_LIFETIME_LIMIT,
      limitReached: false,
      hasAdminGrant: false,
      planTier: null,
    };
  }

  const isPaid = q.plan_tier === "pro" || q.plan_tier === "unlimited";
  const hasAdminGrant =
    q.plan_tier === "free_hosted" && q.limit > FREE_LIFETIME_LIMIT;

  let status: UsageStatus;
  if (isPaid) {
    status = "paid";
  } else if (q.remaining <= 0) {
    status = "free_at_limit";
  } else if (hasAdminGrant) {
    status = "special_access";
  } else {
    status = "free_under_limit";
  }

  return {
    status,
    used: q.used,
    limit: q.limit,
    remaining: q.remaining,
    limitReached: !isPaid && q.remaining <= 0,
    hasAdminGrant,
    planTier: q.plan_tier,
  };
}

export function useEnhancementUsage(): UsageState {
  const [hosted, setHosted] = useState<HostedQuota | null>(null);

  useEffect(() => {
    let alive = true;
    let unlisten: (() => void) | undefined;

    void listen<HostedQuota>("hosted:quota", (event) => {
      if (!alive) return;
      setHosted(event.payload);
    }).then((u) => {
      if (!alive) {
        u();
        return;
      }
      unlisten = u;
    });

    return () => {
      alive = false;
      unlisten?.();
    };
  }, []);

  return useMemo(() => deriveStatus(hosted), [hosted]);
}

export { FREE_LIFETIME_LIMIT };
// Re-export the daily-limit name the old hook exposed so any straggling
// callers don't break — same value, same meaning under the new model.
export const DAILY_LIMIT = FREE_LIFETIME_LIMIT;
