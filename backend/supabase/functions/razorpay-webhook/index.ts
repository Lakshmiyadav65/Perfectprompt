// PerfectPrompt razorpay-webhook edge function.
//
// Razorpay POSTs subscription lifecycle events to this URL whenever one
// of our created subscriptions transitions state. The function:
//   1. Verifies the HMAC-SHA256 signature in `X-Razorpay-Signature`
//      against the raw request body using RAZORPAY_WEBHOOK_SECRET.
//   2. Routes the event by its top-level `event` field.
//   3. Reads notes.user_id from the subscription entity to know which
//      profile row to update (set at create-subscription time).
//   4. Updates plan_tier / subscription_status / current_period_end on
//      the profile. The new consume_daily_quota RPC reads these to
//      decide pro-bypass vs free-daily-quota at enhance time.
//
// Events handled:
//   - subscription.activated → first charge succeeded; set plan_tier='pro'
//   - subscription.charged   → recurring monthly charge; extend period end
//   - subscription.cancelled → user cancelled; keep pro until period ends
//   - subscription.completed → total_count reached; same handling as cancelled
//   - subscription.halted    → payment failed too many times; downgrade now
//   - subscription.paused    → user paused; treat as cancelled-but-keep-period
//   - subscription.resumed   → reverse a previous pause
//
// Other event types get a clean 200 OK no-op so Razorpay doesn't retry.
//
// Idempotency: every UPDATE is keyed on user_id + the event's data is
// effectively a snapshot of current state, so receiving the same event
// twice is safe (just overwrites with identical values).

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

  // Raw body BEFORE parsing — HMAC must match bytes-exact input.
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
  if (!event) {
    return jsonError(400, "missing_event", "Payload had no event field");
  }

  // Only subscription.* events touch profiles. Anything else (orders,
  // refunds, legacy payment_link events) returns 200 OK so Razorpay
  // stops retrying.
  if (!event.startsWith("subscription.")) {
    console.log(`[razorpay-webhook] ignoring non-subscription event=${event}`);
    return ok({ ignored: event });
  }

  const sub = payload?.payload?.subscription?.entity;
  if (!sub) {
    console.error(`[razorpay-webhook] ${event} missing subscription.entity`);
    return jsonError(422, "missing_subscription", "Payload missing subscription entity");
  }

  const subscriptionId = sub.id as string;
  const userId = sub.notes?.user_id as string | undefined;
  if (!userId) {
    console.error(
      `[razorpay-webhook] ${event} (sub=${subscriptionId}) has no notes.user_id`,
    );
    return jsonError(422, "missing_user_id", "Subscription has no user_id in notes");
  }

  const admin = createClient(supabaseUrl, serviceRoleKey, {
    auth: { persistSession: false },
  });

  // Razorpay's `current_end` is the unix timestamp at which the current
  // billing period ends. Convert to ISO for Postgres. Some events
  // (cancelled with cancel_at_cycle_end=false) carry it as null.
  const currentEnd = sub.current_end
    ? new Date((sub.current_end as number) * 1000).toISOString()
    : null;

  switch (event) {
    case "subscription.activated":
    case "subscription.charged":
    case "subscription.resumed": {
      // First-charge or recurring-charge success: grant pro until
      // current_end. Idempotent — receiving the same event twice writes
      // the same values.
      const { error } = await admin
        .from("profiles")
        .update({
          plan_tier:           "pro",
          subscription_status: "active",
          current_period_end:  currentEnd,
          paid_at:             new Date().toISOString(),
          updated_at:          new Date().toISOString(),
        })
        .eq("user_id", userId);
      if (error) {
        console.error(`[razorpay-webhook] ${event} update failed`, error);
        return jsonError(500, "update_failed", "Could not update profile");
      }
      console.log(
        `[razorpay-webhook] ${event} → user=${userId} pro until ${currentEnd}`,
      );
      break;
    }

    case "subscription.cancelled":
    case "subscription.completed":
    case "subscription.paused": {
      // Customer cancelled, plan ran out of cycles, or paused. KEEP them
      // on pro until current_period_end passes — they paid for that
      // window, they should get to use it. The consume_daily_quota RPC
      // lazily downgrades them when the date arrives.
      const { error } = await admin
        .from("profiles")
        .update({
          subscription_status: sub.status ?? event.split(".")[1],
          current_period_end:  currentEnd,
          updated_at:          new Date().toISOString(),
        })
        .eq("user_id", userId);
      if (error) {
        console.error(`[razorpay-webhook] ${event} update failed`, error);
        return jsonError(500, "update_failed", "Could not update profile");
      }
      console.log(
        `[razorpay-webhook] ${event} → user=${userId} keep pro until ${currentEnd}`,
      );
      break;
    }

    case "subscription.halted": {
      // Razorpay halted the subscription after repeated payment failures.
      // Different from a clean cancel: there's no further period the
      // customer paid for. Downgrade immediately so they don't get free
      // pro days while their card is broken.
      const { error } = await admin
        .from("profiles")
        .update({
          plan_tier:           "free_hosted",
          subscription_status: "halted",
          current_period_end:  null,
          updated_at:          new Date().toISOString(),
        })
        .eq("user_id", userId);
      if (error) {
        console.error(`[razorpay-webhook] ${event} update failed`, error);
        return jsonError(500, "update_failed", "Could not update profile");
      }
      console.log(`[razorpay-webhook] ${event} → user=${userId} downgraded`);
      break;
    }

    default: {
      // subscription.authenticated, subscription.pending, etc. — useful
      // for telemetry but no state change.
      console.log(`[razorpay-webhook] noted ${event} user=${userId}`);
      const { error } = await admin
        .from("profiles")
        .update({
          subscription_status: sub.status ?? null,
          updated_at:          new Date().toISOString(),
        })
        .eq("user_id", userId);
      if (error) {
        console.error(`[razorpay-webhook] ${event} status sync failed`, error);
      }
      break;
    }
  }

  return ok({ ok: true, user_id: userId, event });
});

// ---------- helpers ----------

function ok(body: Record<string, unknown>): Response {
  return new Response(JSON.stringify(body), {
    status: 200,
    headers: { ...corsHeaders(), "Content-Type": "application/json" },
  });
}

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
