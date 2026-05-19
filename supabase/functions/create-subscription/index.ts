// PerfectPrompt create-subscription edge function.
//
// Replaces the lifetime-payment create-payment-link function from the
// previous model. Creates a Razorpay Subscription against a pre-existing
// Plan (₹99 monthly), notes the user's uuid in the subscription's
// metadata, and returns the hosted checkout URL.
//
// Auth: requires a Supabase user JWT (the React client invokes via
// supabase.functions.invoke which auto-attaches the access token).
//
// Flow per request:
//   1. Verify JWT → 401 if invalid.
//   2. Refuse if the user is already on 'unlimited' (grandfathered).
//   3. Refuse if the user already has an active subscription
//      (status 'active', or current_period_end still in the future).
//      Returns the existing short_url so the client can reopen the
//      Razorpay portal — useful for "manage my subscription" intent.
//   4. POST https://api.razorpay.com/v1/subscriptions with plan_id
//      from RAZORPAY_PLAN_ID and notes.user_id = <uuid>.
//   5. Persist subscription id + status + short_url on profiles.
//   6. Return { url } for the client to open.

import { createClient } from "https://esm.sh/@supabase/supabase-js@2.45.4";

const RAZORPAY_API_URL = "https://api.razorpay.com/v1/subscriptions";
// 10 years × 12 months. Razorpay requires a finite total_count; this is
// effectively "until they cancel" — well past any plausible product lifespan.
const TOTAL_COUNT = 120;

Deno.serve(async (req) => {
  if (req.method === "OPTIONS") {
    return new Response(null, { headers: corsHeaders() });
  }
  if (req.method !== "POST") {
    return jsonError(405, "method_not_allowed", "Use POST");
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
  const planId         = mustEnv("RAZORPAY_PLAN_ID");

  const admin = createClient(supabaseUrl, serviceRoleKey, {
    auth: { persistSession: false },
  });

  const { data: userData, error: userErr } = await admin.auth.getUser(userToken);
  if (userErr || !userData?.user) {
    return jsonError(401, "invalid_token", "Token rejected");
  }
  const userId = userData.user.id;
  const userEmail = userData.user.email ?? null;

  // ---- Idempotency + existing-subscription handling ----
  const { data: profile, error: profileErr } = await admin
    .from("profiles")
    .select("plan_tier, subscription_status, current_period_end, subscription_short_url")
    .eq("user_id", userId)
    .maybeSingle();
  if (profileErr) {
    console.error("[create-subscription] profile fetch failed", profileErr);
    return jsonError(500, "profile_error", "Could not read profile");
  }
  if (profile?.plan_tier === "unlimited") {
    return jsonError(409, "already_unlimited",
      "Account is already on unlimited access — no subscription needed");
  }
  if (profile?.subscription_status === "active" && profile.subscription_short_url) {
    // Active subscription — bounce them back to the existing portal URL
    // so they can manage / view / cancel without us starting a duplicate.
    return new Response(
      JSON.stringify({
        url: profile.subscription_short_url,
        existing: true,
      }),
      {
        status: 200,
        headers: { ...corsHeaders(), "Content-Type": "application/json" },
      },
    );
  }

  // ---- Razorpay API call ----
  const auth = "Basic " + btoa(`${razorpayKeyId}:${razorpaySecret}`);

  const body = {
    plan_id: planId,
    total_count: TOTAL_COUNT,
    customer_notify: 1,
    notes: {
      user_id: userId,
      email: userEmail ?? "",
    },
  };

  let resp: Response;
  try {
    resp = await fetch(RAZORPAY_API_URL, {
      method: "POST",
      headers: {
        Authorization: auth,
        "Content-Type": "application/json",
      },
      body: JSON.stringify(body),
    });
  } catch (e) {
    console.error("[create-subscription] Razorpay fetch failed", e);
    return jsonError(502, "razorpay_unreachable", "Could not reach Razorpay");
  }

  if (!resp.ok) {
    const errBody = await resp.text();
    console.error("[create-subscription] Razorpay non-2xx", resp.status, errBody);
    return jsonError(502, "razorpay_error", `Razorpay returned ${resp.status}`, {
      detail: errBody.slice(0, 500),
    });
  }

  const parsed = await resp.json() as {
    id?: string;
    short_url?: string;
    status?: string;
  };
  if (!parsed.id || !parsed.short_url) {
    return jsonError(502, "razorpay_invalid",
      "Razorpay response missing id or short_url");
  }

  // Persist subscription metadata so the webhook can match incoming
  // events back to this user even if the notes round-trip ever drops.
  const { error: updateErr } = await admin
    .from("profiles")
    .update({
      razorpay_subscription_id: parsed.id,
      subscription_status:      parsed.status ?? "created",
      subscription_short_url:   parsed.short_url,
      updated_at:               new Date().toISOString(),
    })
    .eq("user_id", userId);
  if (updateErr) {
    // Don't fail the request — the subscription exists in Razorpay,
    // the customer will pay, the webhook will land the data later.
    console.error("[create-subscription] profile persist failed", updateErr);
  }

  return new Response(
    JSON.stringify({
      url: parsed.short_url,
      subscription_id: parsed.id,
      existing: false,
    }),
    {
      status: 200,
      headers: { ...corsHeaders(), "Content-Type": "application/json" },
    },
  );
});

// ---------- helpers ----------

function corsHeaders(): Record<string, string> {
  return {
    "Access-Control-Allow-Origin":  "*",
    "Access-Control-Allow-Methods": "POST, OPTIONS",
    "Access-Control-Allow-Headers": "authorization, content-type",
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
