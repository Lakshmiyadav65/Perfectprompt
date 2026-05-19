// PerfectPrompt verify-subscription edge function.
//
// Self-healing endpoint that reconciles a user's profile row against
// Razorpay's authoritative subscription state. Used when the webhook
// missed an event (network drop, retry timeout, signature mismatch
// from a half-deployed function) so the customer doesn't have to email
// support to get unstuck.
//
// Auth: requires a Supabase user JWT.
//
// Flow:
//   1. Read user's razorpay_subscription_id from profile.
//   2. If null → return { has_subscription: false }. Client renders
//      "no subscription on file" state.
//   3. GET /v1/subscriptions/{sub_id} from Razorpay.
//   4. Map Razorpay's status + current_end to our DB columns:
//        - active / authenticated → plan_tier='pro', period_end=current_end
//        - cancelled / completed / paused → keep pro until period passes
//        - halted → downgrade immediately
//        - created / pending → just sync status
//   5. UPDATE profile, return the synced state.
//
// Idempotent — running it twice for the same subscription is a no-op
// on the second call. Safe to wire to a periodic cron later.

import { createClient } from "https://esm.sh/@supabase/supabase-js@2.45.4";

const RAZORPAY_API = "https://api.razorpay.com/v1/subscriptions";

interface RazorpaySubscription {
  id: string;
  status: string;
  current_end?: number | null;
  notes?: Record<string, string> | null;
}

Deno.serve(async (req) => {
  if (req.method === "OPTIONS") {
    return new Response(null, { headers: corsHeaders() });
  }
  if (req.method !== "POST" && req.method !== "GET") {
    return jsonError(405, "method_not_allowed", "Use POST or GET");
  }

  // ---- Auth ----
  const authHeader = req.headers.get("Authorization") ?? "";
  if (!authHeader.startsWith("Bearer ")) {
    return jsonError(401, "unauthenticated", "Missing bearer token");
  }
  const userToken = authHeader.slice("Bearer ".length).trim();

  const supabaseUrl    = mustEnv("SUPABASE_URL");
  const serviceRoleKey = mustEnv("SUPABASE_SERVICE_ROLE_KEY");
  const razorpayKeyId  = mustEnv("RAZORPAY_KEY_ID");
  const razorpaySecret = mustEnv("RAZORPAY_KEY_SECRET");

  const admin = createClient(supabaseUrl, serviceRoleKey, {
    auth: { persistSession: false },
  });

  const { data: userData, error: userErr } = await admin.auth.getUser(userToken);
  if (userErr || !userData?.user) {
    return jsonError(401, "invalid_token", "Token rejected");
  }
  const userId = userData.user.id;

  // ---- Look up subscription_id ----
  const { data: profile, error: profileErr } = await admin
    .from("profiles")
    .select("plan_tier, razorpay_subscription_id, subscription_status, current_period_end")
    .eq("user_id", userId)
    .maybeSingle();
  if (profileErr) {
    console.error("[verify-subscription] profile fetch failed", profileErr);
    return jsonError(500, "profile_error", "Could not read profile");
  }
  if (profile?.plan_tier === "unlimited") {
    // Admin-granted, doesn't need Razorpay verification.
    return ok({
      plan_tier: "unlimited",
      subscription_status: null,
      current_period_end: null,
      has_subscription: false,
      synced: false,
      message: "Account is on admin-granted unlimited access",
    });
  }
  const subscriptionId = profile?.razorpay_subscription_id;
  if (!subscriptionId) {
    return ok({
      plan_tier: profile?.plan_tier ?? "free_hosted",
      subscription_status: null,
      current_period_end: null,
      has_subscription: false,
      synced: false,
      message: "No subscription on file",
    });
  }

  // ---- Fetch from Razorpay ----
  const auth = "Basic " + btoa(`${razorpayKeyId}:${razorpaySecret}`);
  let rzpResp: Response;
  try {
    rzpResp = await fetch(`${RAZORPAY_API}/${subscriptionId}`, {
      method: "GET",
      headers: { Authorization: auth },
    });
  } catch (e) {
    console.error("[verify-subscription] Razorpay fetch failed", e);
    return jsonError(502, "razorpay_unreachable", "Could not reach Razorpay");
  }

  if (rzpResp.status === 404) {
    // Razorpay forgot the subscription (rare; ID may have been wiped
    // server-side). Treat as no-subscription so the user can start fresh.
    return ok({
      plan_tier: profile?.plan_tier ?? "free_hosted",
      subscription_status: null,
      current_period_end: null,
      has_subscription: false,
      synced: false,
      message: "Subscription not found on Razorpay",
    });
  }
  if (!rzpResp.ok) {
    const errBody = await rzpResp.text();
    console.error("[verify-subscription] Razorpay non-2xx", rzpResp.status, errBody);
    return jsonError(502, "razorpay_error", `Razorpay returned ${rzpResp.status}`);
  }

  const rzpSub = await rzpResp.json() as RazorpaySubscription;
  const currentEnd = rzpSub.current_end
    ? new Date(rzpSub.current_end * 1000).toISOString()
    : null;

  // ---- Decide what state to write ----
  // The matrix mirrors razorpay-webhook's event-driven version, just
  // collapsed into a single function based on the subscription's
  // current snapshot.
  const status = rzpSub.status ?? "unknown";
  let newPlanTier: string;
  if (status === "active" || status === "authenticated") {
    newPlanTier = "pro";
  } else if (status === "cancelled" || status === "completed" || status === "paused") {
    // Keep them on pro until current_end passes (they paid for the window).
    newPlanTier = currentEnd && new Date(currentEnd).getTime() > Date.now()
      ? "pro"
      : "free_hosted";
  } else if (status === "halted") {
    // Payment failures — revoke immediately.
    newPlanTier = "free_hosted";
  } else {
    // created / pending / expired / unknown — don't grant pro.
    newPlanTier = "free_hosted";
  }

  // ---- Sync ----
  const updates: Record<string, unknown> = {
    subscription_status: status,
    current_period_end:  currentEnd,
    updated_at:          new Date().toISOString(),
  };
  if (newPlanTier !== profile?.plan_tier) {
    updates.plan_tier = newPlanTier;
    if (newPlanTier === "pro" && !profile?.current_period_end) {
      updates.paid_at = new Date().toISOString();
    }
  }

  const { error: updateErr } = await admin
    .from("profiles")
    .update(updates)
    .eq("user_id", userId);
  if (updateErr) {
    console.error("[verify-subscription] update failed", updateErr);
    return jsonError(500, "update_failed", "Could not update profile");
  }

  console.log(
    `[verify-subscription] user=${userId} sub=${subscriptionId} rzp_status=${status} → plan_tier=${newPlanTier}`,
  );

  return ok({
    plan_tier: newPlanTier,
    subscription_status: status,
    current_period_end: currentEnd,
    has_subscription: true,
    synced: true,
  });
});

// ---------- helpers ----------

function ok(body: Record<string, unknown>): Response {
  return new Response(JSON.stringify(body), {
    status: 200,
    headers: { ...corsHeaders(), "Content-Type": "application/json" },
  });
}

function corsHeaders(): Record<string, string> {
  return {
    "Access-Control-Allow-Origin":  "*",
    "Access-Control-Allow-Methods": "POST, GET, OPTIONS",
    "Access-Control-Allow-Headers":
      "authorization, content-type, apikey, x-client-info",
  };
}

function jsonError(
  status: number,
  code: string,
  message: string,
  extra: Record<string, unknown> = {},
): Response {
  return new Response(
    JSON.stringify({ error: code, message, ...extra }),
    {
      status,
      headers: { ...corsHeaders(), "Content-Type": "application/json" },
    },
  );
}

function mustEnv(name: string): string {
  const v = Deno.env.get(name);
  if (!v) throw new Error(`Missing required env: ${name}`);
  return v;
}
