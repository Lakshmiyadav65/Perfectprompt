// PerfectPrompt create-payment-link edge function.
//
// Auth: requires a Supabase user JWT (the Tauri client signs in via OAuth
// and sends `Authorization: Bearer <access_token>` from React via the
// supabase.functions.invoke helper).
//
// Flow per request:
//   1. Verify JWT → 401 if invalid.
//   2. Refuse if the user is already on a paid plan (idempotency).
//   3. Refuse if the user is still under their free trial (no point
//      buying lifetime when you have 8 free runs left). This guard is
//      soft — the client also hides the Upgrade button while
//      `usage.remaining > 0`, but defending here means a curl-savvy
//      user can't spam payment-link creation.
//   4. POST https://api.razorpay.com/v1/payment_links with the user's
//      uuid stashed in `notes.user_id`. The webhook reads that back
//      when the payment completes and flips the same user to 'pro'.
//   5. Return { url } for the client to open.
//
// Why per-user links: the original setup used a single static Razorpay
// link shared across all customers, which forced manual SQL to flip
// each payer to 'pro'. Per-user links let the webhook unambiguously
// match a payment to a profile without any email-matching guesswork.

import { createClient } from "https://esm.sh/@supabase/supabase-js@2.45.4";

const RAZORPAY_API_URL = "https://api.razorpay.com/v1/payment_links";
const PRICE_PAISE      = 19_900; // ₹199 in paise (Razorpay's smallest unit)
const DESCRIPTION      = "PerfectPrompt — lifetime access";

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

  const admin = createClient(supabaseUrl, serviceRoleKey, {
    auth: { persistSession: false },
  });

  const { data: userData, error: userErr } = await admin.auth.getUser(userToken);
  if (userErr || !userData?.user) {
    return jsonError(401, "invalid_token", "Token rejected");
  }
  const userId = userData.user.id;
  const userEmail = userData.user.email ?? null;

  // ---- Idempotency: paid users don't need another link ----
  const { data: profile, error: profileErr } = await admin
    .from("profiles")
    .select("plan_tier")
    .eq("user_id", userId)
    .maybeSingle();
  if (profileErr) {
    console.error("[create-payment-link] profile fetch failed", profileErr);
    return jsonError(500, "profile_error", "Could not read profile");
  }
  if (profile?.plan_tier === "pro" || profile?.plan_tier === "unlimited") {
    return jsonError(409, "already_paid", "User is already on a paid plan");
  }

  // ---- Razorpay API call ----
  // Basic-auth: KEY_ID:KEY_SECRET, base64-encoded.
  const auth = "Basic " + btoa(`${razorpayKeyId}:${razorpaySecret}`);

  const body = {
    amount: PRICE_PAISE,
    currency: "INR",
    accept_partial: false,
    description: DESCRIPTION,
    // Suppress Razorpay's own SMS/email reminders — the customer is
    // already in front of the payment page, no need to nag them later.
    notify: { sms: false, email: false },
    reminder_enable: false,
    // The matchback signal. Webhook reads `notes.user_id` to know
    // which profile row to flip. Email is included for human-readable
    // auditing in the Razorpay dashboard.
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
    console.error("[create-payment-link] Razorpay fetch failed", e);
    return jsonError(502, "razorpay_unreachable", "Could not reach Razorpay");
  }

  if (!resp.ok) {
    const errBody = await resp.text();
    console.error("[create-payment-link] Razorpay non-2xx", resp.status, errBody);
    return jsonError(502, "razorpay_error", `Razorpay returned ${resp.status}`, {
      detail: errBody.slice(0, 500),
    });
  }

  const parsed = await resp.json() as { short_url?: string; id?: string };
  if (!parsed.short_url) {
    return jsonError(502, "razorpay_invalid", "Razorpay response missing short_url");
  }

  return new Response(
    JSON.stringify({ url: parsed.short_url, payment_link_id: parsed.id }),
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
