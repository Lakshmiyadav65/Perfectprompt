import { useEffect, useMemo, useState } from "react";
import { listen } from "@tauri-apps/api/event";

import { isSupabaseConfigured, supabase } from "../lib/supabase";

/// Lifetime free-trial tracker for the hosted enhancement tier.
///
/// Replaces the previous daily-quota model. Every signed-in user starts
/// with 10 free enhancements; admin grants (extra_granted on the profile
/// row) bump that ceiling per-user; paid users (plan_tier in 'pro' /
/// 'unlimited') bypass the cap entirely.
///
/// Two state sources, merged into one snapshot:
///   1. Initial read on mount — direct SELECT against public.profiles
///      using the signed-in user's JWT. Lets the sidebar render an
///      accurate "N / 10" before the user has triggered any enhancement
///      this session. RLS gates the row to the user themselves.
///   2. Live updates — every `/enhance` call emits `hosted:quota` from
///      the Rust pipeline (Stage D). The listener overwrites the
///      initial-read snapshot with the freshest server-authoritative
///      numbers.
///
/// BYOK / signed-out users have no quota at all — they pay Groq directly
/// out-of-pocket. The hook returns `status: "byok"` and the sidebar
/// suppresses the usage card entirely.

const FREE_LIFETIME_LIMIT = 10;
const PAID_TIERS: ReadonlyArray<string> = ["pro", "unlimited"];

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

  const isPaid = PAID_TIERS.includes(q.plan_tier);
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

/// Build a HostedQuota from a raw profiles row. Mirrors the math
/// consume_lifetime_quota does server-side: paid tiers report INT_MAX as
/// the cap and `used` stays at lifetime_used for display; free tier's
/// cap is 10 + extra_granted.
function quotaFromProfile(row: {
  lifetime_used: number | null;
  extra_granted: number | null;
  plan_tier: string | null;
}): HostedQuota {
  const tier = (row.plan_tier ?? "free_hosted") as PlanTier;
  const used = row.lifetime_used ?? 0;
  const isPaid = PAID_TIERS.includes(tier);
  if (isPaid) {
    return {
      used,
      limit: 2_147_483_647,
      remaining: 2_147_483_647,
      plan_tier: tier,
      resets_at: null,
    };
  }
  const limit = FREE_LIFETIME_LIMIT + (row.extra_granted ?? 0);
  return {
    used,
    limit,
    remaining: Math.max(0, limit - used),
    plan_tier: tier,
    resets_at: null,
  };
}

export function useEnhancementUsage(): UsageState {
  const [hosted, setHosted] = useState<HostedQuota | null>(null);

  // Initial read on mount — snapshots the user's current quota so the
  // sidebar renders an accurate state before any enhancement this
  // session. Re-fires when auth state changes so signing in / out
  // updates the card without a full app reload.
  useEffect(() => {
    if (!isSupabaseConfigured) return;
    let alive = true;

    const fetchProfile = async () => {
      const { data: userResp, error: userErr } = await supabase.auth.getUser();
      if (!alive) return;
      if (userErr || !userResp?.user) {
        // Signed-out → BYOK. Clear any stale hosted snapshot.
        setHosted(null);
        return;
      }
      const userId = userResp.user.id;
      const { data: profile, error: profileErr } = await supabase
        .from("profiles")
        .select("lifetime_used, extra_granted, plan_tier")
        .eq("user_id", userId)
        .maybeSingle();
      if (!alive) return;
      if (profileErr) {
        console.error("[useEnhancementUsage] profile fetch failed:", profileErr);
        return;
      }
      if (!profile) {
        // The on_auth_user_created trigger should have populated this;
        // treat a missing row as a fresh user at 0/10.
        setHosted(
          quotaFromProfile({
            lifetime_used: 0,
            extra_granted: 0,
            plan_tier: "free_hosted",
          }),
        );
        return;
      }
      setHosted(quotaFromProfile(profile));
    };

    void fetchProfile();

    // React to auth changes — sign-in flips byok → free_under_limit;
    // sign-out flips back.
    const { data: sub } = supabase.auth.onAuthStateChange(() => {
      void fetchProfile();
    });

    return () => {
      alive = false;
      sub.subscription.unsubscribe();
    };
  }, []);

  // Live updates from the hosted /enhance pipeline. Authoritative —
  // overwrites the initial snapshot once a real call has been made.
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
