import { useEffect, useMemo, useState } from "react";
import { listen } from "@tauri-apps/api/event";

import { isSupabaseConfigured, supabase } from "../lib/supabase";

/// Daily-free + monthly-pro tracker.
///
/// Replaces the brief lifetime-trial model. Tiers:
///   - free_hosted: 10 enhancements per IST day (+ extra_granted bonus)
///   - pro: unlimited while current_period_end > now() (active Razorpay subscription)
///   - unlimited: admin-grandfathered, always bypass
///
/// Sources merged into one snapshot:
///   1. Mount-time SELECT from public.profiles (so the sidebar paints
///      correct state before the first enhancement).
///   2. Live `hosted:quota` events from the Rust pipeline (Stage D)
///      after every /enhance call — the server is authoritative.
///
/// BYOK / signed-out users render `status: "byok"` and the sidebar
/// suppresses the usage card.

const DAILY_FREE_LIMIT = 10;

type PlanTier = "free_hosted" | "pro" | "unlimited" | string;

/// Wire shape emitted on `hosted:quota` after every /enhance call.
/// Mirror of the new consume_daily_quota response. `limit`/`remaining`
/// are nullable because pro/unlimited tiers don't have a meaningful
/// numeric ceiling.
interface HostedQuota {
  used: number;
  limit: number | null;
  remaining: number | null;
  plan_tier: PlanTier;
  subscription_active?: boolean;
  /// Next midnight Asia/Kolkata as ISO. Null for pro/unlimited (no reset).
  resets_at?: string | null;
}

export type UsageStatus =
  /// Free tier, under today's cap. Can enhance.
  | "free_under_limit"
  /// Free tier with extra_granted > 0 — same as free_under_limit but
  /// the UI surfaces "Special access" so the user understands why
  /// their cap is higher than the default 10.
  | "special_access"
  /// Free tier, today's cap is hit. Pipeline blocked until midnight IST
  /// or until they subscribe.
  | "free_at_limit"
  /// Pro tier with an active subscription — unlimited until period ends.
  | "pro_active"
  /// Pro tier whose subscription expired and hasn't been renewed.
  /// Equivalent to free_hosted for quota purposes but the UI shows
  /// "Subscription lapsed — resubscribe" rather than the cleaner
  /// free-tier copy.
  | "pro_lapsed"
  /// Admin-grandfathered: always bypass (Lakshmi after Phase 0).
  | "unlimited"
  /// Not signed in / no Supabase backend.
  | "byok";

export interface UsageState {
  status: UsageStatus;
  /// How many enhancements consumed in the current IST day. Always 0
  /// for pro/unlimited (their counter doesn't tick).
  used: number;
  /// Daily ceiling for free users (10 + extra_granted). null for
  /// pro/unlimited where the concept doesn't apply.
  limit: number | null;
  /// Daily remaining for free users. null for pro/unlimited.
  remaining: number | null;
  /// True if the user is currently blocked. False for pro/unlimited
  /// regardless of `used`. Pipeline / hotkey gate on this.
  limitReached: boolean;
  /// True iff free_hosted user has extra_granted > 0.
  hasAdminGrant: boolean;
  /// Active Razorpay subscription? True for pro_active.
  subscriptionActive: boolean;
  /// When the subscription's current paid period ends. ISO timestamp.
  /// Null when no subscription or expired.
  currentPeriodEnd: string | null;
  /// Razorpay subscription short_url so the user can revisit / cancel.
  /// Null when no subscription created yet.
  subscriptionShortUrl: string | null;
  /// Next midnight IST (ISO) for free users. Used to render
  /// "resets in N hours" countdown. Null for pro/unlimited.
  resetsAt: string | null;
  planTier: PlanTier | null;
}

function deriveStatus(q: HostedQuota | null, extras: {
  currentPeriodEnd: string | null;
  subscriptionShortUrl: string | null;
}): UsageState {
  if (!q) {
    return {
      status: "byok",
      used: 0,
      limit: DAILY_FREE_LIMIT,
      remaining: DAILY_FREE_LIMIT,
      limitReached: false,
      hasAdminGrant: false,
      subscriptionActive: false,
      currentPeriodEnd: null,
      subscriptionShortUrl: null,
      resetsAt: null,
      planTier: null,
    };
  }

  const isUnlimited = q.plan_tier === "unlimited";
  const isProActive = q.plan_tier === "pro" && (q.subscription_active === true);
  const isProLapsed = q.plan_tier === "pro" && (q.subscription_active !== true);
  const hasAdminGrant =
    q.plan_tier === "free_hosted" && (q.limit ?? 0) > DAILY_FREE_LIMIT;
  const remainingNum = q.remaining ?? 0;

  let status: UsageStatus;
  if (isUnlimited) status = "unlimited";
  else if (isProActive) status = "pro_active";
  else if (isProLapsed) status = "pro_lapsed";
  else if (q.plan_tier === "free_hosted" && remainingNum <= 0) status = "free_at_limit";
  else if (hasAdminGrant) status = "special_access";
  else status = "free_under_limit";

  return {
    status,
    used: q.used,
    limit: q.limit,
    remaining: q.remaining,
    limitReached: status === "free_at_limit" || status === "pro_lapsed",
    hasAdminGrant,
    subscriptionActive: isProActive,
    currentPeriodEnd: extras.currentPeriodEnd,
    subscriptionShortUrl: extras.subscriptionShortUrl,
    resetsAt: q.resets_at ?? null,
    planTier: q.plan_tier,
  };
}

/// Mount-time profile read → HostedQuota. Math mirrors what
/// consume_daily_quota does server-side, EXCEPT the daily counter
/// (lifetime_used is no longer the field; daily_usage is a separate
/// table the hook doesn't try to read). For free users we don't know
/// "used today" until the first /enhance call, so we surface 0 used
/// and the server's authoritative count overwrites it on the next
/// hosted:quota event.
function quotaFromProfile(row: {
  extra_granted: number | null;
  plan_tier: string | null;
  current_period_end: string | null;
}): HostedQuota {
  const tier = (row.plan_tier ?? "free_hosted") as PlanTier;
  const isUnlimited = tier === "unlimited";
  const periodEnd = row.current_period_end ? new Date(row.current_period_end) : null;
  const isProActive = tier === "pro" && periodEnd !== null && periodEnd.getTime() > Date.now();

  if (isUnlimited || isProActive) {
    return {
      used: 0,
      limit: null,
      remaining: null,
      plan_tier: tier,
      subscription_active: isProActive || isUnlimited,
      resets_at: null,
    };
  }

  const dailyLimit = DAILY_FREE_LIMIT + (row.extra_granted ?? 0);
  return {
    used: 0, // unknown until /enhance call returns the real count
    limit: dailyLimit,
    remaining: dailyLimit,
    plan_tier: tier,
    subscription_active: false,
    resets_at: null,
  };
}

export function useEnhancementUsage(): UsageState {
  const [hosted, setHosted] = useState<HostedQuota | null>(null);
  const [currentPeriodEnd, setCurrentPeriodEnd] = useState<string | null>(null);
  const [subscriptionShortUrl, setSubscriptionShortUrl] = useState<string | null>(null);

  // Initial profile read so sidebar paints on launch.
  useEffect(() => {
    if (!isSupabaseConfigured) return;
    let alive = true;

    const fetchProfile = async () => {
      const { data: userResp, error: userErr } = await supabase.auth.getUser();
      if (!alive) return;
      if (userErr || !userResp?.user) {
        setHosted(null);
        setCurrentPeriodEnd(null);
        setSubscriptionShortUrl(null);
        return;
      }
      const userId = userResp.user.id;
      const { data: profile, error: profileErr } = await supabase
        .from("profiles")
        .select("plan_tier, extra_granted, current_period_end, subscription_short_url")
        .eq("user_id", userId)
        .maybeSingle();
      if (!alive) return;
      if (profileErr) {
        console.error("[useEnhancementUsage] profile fetch failed:", profileErr);
        return;
      }
      if (!profile) {
        setHosted(quotaFromProfile({
          extra_granted: 0,
          plan_tier: "free_hosted",
          current_period_end: null,
        }));
        return;
      }
      setHosted(quotaFromProfile(profile));
      setCurrentPeriodEnd(profile.current_period_end ?? null);
      setSubscriptionShortUrl(profile.subscription_short_url ?? null);
    };

    void fetchProfile();

    const { data: sub } = supabase.auth.onAuthStateChange(() => {
      void fetchProfile();
    });

    return () => {
      alive = false;
      sub.subscription.unsubscribe();
    };
  }, []);

  // Live hosted:quota events from Rust after each /enhance call.
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

  return useMemo(
    () => deriveStatus(hosted, { currentPeriodEnd, subscriptionShortUrl }),
    [hosted, currentPeriodEnd, subscriptionShortUrl],
  );
}

export { DAILY_FREE_LIMIT };
// Back-compat alias for older callers still importing DAILY_LIMIT.
export const DAILY_LIMIT = DAILY_FREE_LIMIT;
