// PerfectPrompt razorpay-webhook edge function.
//
// Razorpay POSTs `payment_link.paid` (and other) events to this URL
// whenever one of our created payment links is completed. The function:
//   1. Verifies the HMAC-SHA256 signature in `X-Razorpay-Signature`
//      against the raw request body using RAZORPAY_WEBHOOK_SECRET.
//      Any mismatch → 401 (someone forging a webhook).
//   2. Filters down to `payment_link.paid` events. Other event types
//      (payment.authorized, etc.) are 200-OK no-ops so Razorpay
//      doesn't keep retrying them.
//   3. Extracts `notes.user_id` from the payment_link entity — that's
//      the Supabase auth.users uuid the create-payment-link function
//      stashed there at link creation time.
//   4. Flips public.profiles.plan_tier = 'pro' and paid_at = now() for
//      that user. Idempotent — running it twice for the same user is
//      a no-op (already pro), so Razorpay's at-least-once delivery is
//      safe.
//
// The webhook URL Razorpay will POST to is:
//   https://<your-project>.supabase.co/functions/v1/razorpay-webhook
//
// Set RAZORPAY_WEBHOOK_SECRET as a Supabase function secret (NOT in
// the edge function code, NOT in a .env file checked into git). The
// secret is generated when you create the webhook in Razorpay's
// dashboard — copy it the one time it's shown to you.

import { createClient } from "https://esm.sh/@supabase/supabase-js@2.45.4";

Deno.serve(async (req) => {
  if (req.method === "OPTIONS") {
    return new Response(null, { headers: corsHeaders() });
  }
  if (req.method !== "POST") {
    return jsonError(405, "method_not_allowed", "Use POST");
  }

  const webhookSecret  = Deno.env.get("RAZORPAY_WEBHOOK_SECRET");
  const supabaseUrl    = mustEnv("SUPABASE_URL");
  const serviceRoleKey = mustEnv("SUPABASE_SERVICE_ROLE_KEY");
  if (!webhookSecret) {
    console.error("[razorpay-webhook] RAZORPAY_WEBHOOK_SECRET not set");
    return jsonError(500, "config_error", "Webhook not configured");
  }

  // Read the raw body BEFORE parsing — HMAC must run over bytes-exact
  // input, and json() would re-encode whitespace etc.
  const rawBody = await req.text();
  const signature = req.headers.get("X-Razorpay-Signature") ?? "";
  if (!signature) {
    return jsonError(401, "missing_signature", "Missing X-Razorpay-Signature");
  }

  const valid = await verifyHmac(rawBody, signature, webhookSecret);
  if (!valid) {
    console.warn("[razorpay-webhook] signature mismatch");
    return jsonError(401, "bad_signature", "Signature verification failed");
  }

  let payload: any;
  try {
    payload = JSON.parse(rawBody);
  } catch {
    return jsonError(400, "bad_json", "Body is not valid JSON");
  }

  const event = payload?.event as string | undefined;
  // Only act on payment_link.paid. The webhook is configured to
  // subscribe only to that event in the Razorpay dashboard, but we
  // double-check defensively so noise events get a clean 200 OK.
  if (event !== "payment_link.paid") {
    console.log(`[razorpay-webhook] ignoring event=${event}`);
    return new Response(JSON.stringify({ ignored: event ?? "unknown" }), {
      status: 200,
      headers: { ...corsHeaders(), "Content-Type": "application/json" },
    });
  }

  const link = payload?.payload?.payment_link?.entity;
  const userId = link?.notes?.user_id as string | undefined;
  const paymentLinkId = link?.id as string | undefined;
  if (!userId) {
    console.error(
      "[razorpay-webhook] payment_link.paid missing notes.user_id",
      paymentLinkId,
    );
    return jsonError(422, "missing_user_id", "Webhook payload had no user_id");
  }

  // ---- Flip the profile ----
  const admin = createClient(supabaseUrl, serviceRoleKey, {
    auth: { persistSession: false },
  });

  const { error: updateErr } = await admin
    .from("profiles")
    .update({
      plan_tier:  "pro",
      paid_at:    new Date().toISOString(),
      updated_at: new Date().toISOString(),
    })
    .eq("user_id", userId);

  if (updateErr) {
    console.error("[razorpay-webhook] update profile failed", updateErr);
    // Return non-2xx so Razorpay retries — transient DB blips will
    // self-heal on the next attempt.
    return jsonError(500, "update_failed", "Could not update profile");
  }

  console.log(
    `[razorpay-webhook] flipped user=${userId} to pro (payment_link=${paymentLinkId})`,
  );

  return new Response(JSON.stringify({ ok: true, user_id: userId }), {
    status: 200,
    headers: { ...corsHeaders(), "Content-Type": "application/json" },
  });
});

// ---------- helpers ----------

/// HMAC-SHA256 of `body` keyed with `secret`, compared in constant
/// time to `expected` (hex). Razorpay's webhook signature is hex-
/// encoded, lowercase, no prefix.
async function verifyHmac(
  body: string,
  expected: string,
  secret: string,
): Promise<boolean> {
  const enc = new TextEncoder();
  const key = await crypto.subtle.importKey(
    "raw",
    enc.encode(secret),
    { name: "HMAC", hash: "SHA-256" },
    false,
    ["sign"],
  );
  const sigBytes = await crypto.subtle.sign("HMAC", key, enc.encode(body));
  const computed = Array.from(new Uint8Array(sigBytes))
    .map((b) => b.toString(16).padStart(2, "0"))
    .join("");
  return constantTimeEqual(computed, expected);
}

/// Constant-time string compare so a side-channel timing attack can't
/// recover the secret one byte at a time.
function constantTimeEqual(a: string, b: string): boolean {
  if (a.length !== b.length) return false;
  let diff = 0;
  for (let i = 0; i < a.length; i++) diff |= a.charCodeAt(i) ^ b.charCodeAt(i);
  return diff === 0;
}

function corsHeaders(): Record<string, string> {
  return {
    "Access-Control-Allow-Origin":  "*",
    "Access-Control-Allow-Methods": "POST, OPTIONS",
    "Access-Control-Allow-Headers": "authorization, content-type, x-razorpay-signature",
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
