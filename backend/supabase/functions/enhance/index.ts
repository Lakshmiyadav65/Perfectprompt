// PerfectPrompt /enhance edge function.
//
// Auth: requires a Supabase user JWT (the Tauri client signs in via OAuth,
// then sends `Authorization: Bearer <access_token>`).
//
// Flow per request:
//   1. Verify JWT  → 401 if invalid.
//   2. Validate input (route, length cap).
//   3. consume_daily_quota(user_id) — atomic increment-if-under-cap.
//      Daily limit is 10 + extra_granted, IST-bucketed.
//      Pro users (plan_tier='pro' with current_period_end > now()): bypass.
//      Unlimited users (admin grandfathered, plan_tier='unlimited'): bypass.
//   4. If allowed: call Groq with the route-specific system prompt.
//   5. Log enhancements row (fire-and-forget; no prompt text stored).
//   6. Return { enhanced_text, quota }.
//
// Quota policy: counts attempts, not successes. A failed Groq call still
// consumes one unit. Documented trade-off — abuse protection > UX nicety
// for v1. Revisit when we have real upstream-failure-rate data.

import { createClient } from "https://esm.sh/@supabase/supabase-js@2.45.4";
import { SYSTEM_PROMPTS, type Route } from "./_prompts.ts";

const GROQ_URL         = "https://api.groq.com/openai/v1/chat/completions";
const GROQ_MODEL       = "llama-3.3-70b-versatile";
const MAX_INPUT_CHARS  = 8000;
const LLM_TIMEOUT_MS   = 30_000;
// Project Knowledge revamp (Layer 4) — accept a client-built <context>
// block so signed-in (hosted) users get the same project-aware enhance
// experience as BYOK users. The cap is set above DIGEST_MAX_BYTES
// (120 KB on the client) to leave room for the short-facts block and
// any future expansion. Rejects oversized payloads with HTTP 413.
const MAX_CONTEXT_BLOCK_CHARS = 150_000;
const ROUTES: readonly Route[] = [
  "code",
  "writing",
  "generic",
  "polish",
  // Project Knowledge rethink — the hosted path for the one-shot
  // PROJECT.md generator. The Tauri client fires this once when
  // a digest is built (or refreshed), then injects the resulting
  // 2 KB markdown on every subsequent enhance instead of the
  // full digest.
  "project_summary",
];

// Shape returned by public.consume_daily_quota(uuid). Adds
// subscription_active so the client can render "Pro · renews in N days"
// distinctly from "Free 8/10 today".
interface QuotaRow {
  allowed: boolean;
  used: number;
  quota_limit: number;
  plan_tier: string;
  subscription_active: boolean;
}

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
  const groqKey        = Deno.env.get("HOSTED_GROQ_API_KEY");
  if (!groqKey) {
    return jsonError(500, "config_error", "Server missing HOSTED_GROQ_API_KEY");
  }

  const admin = createClient(supabaseUrl, serviceRoleKey, {
    auth: { persistSession: false },
  });

  const { data: userData, error: userErr } = await admin.auth.getUser(userToken);
  if (userErr || !userData?.user) {
    return jsonError(401, "invalid_token", "Token rejected");
  }
  const userId = userData.user.id;

  // ---- Body validation ----
  // Project Knowledge revamp (Layer 4): `context_block` is the client-
  // built <context>...</context> wrapper (including the repository
  // digest when present). The server treats it as OPAQUE — no parsing,
  // no validation of the inner XML. We just prepend it to the user
  // message before the <input> tag. The client owns budget enforcement
  // via DIGEST_MAX_BYTES; we only enforce a generous safety cap.
  let body: { input_text?: string; route?: string; context_block?: string };
  try {
    body = await req.json();
  } catch {
    return jsonError(400, "bad_json", "Body must be valid JSON");
  }

  const input = (body.input_text ?? "").trim();
  if (!input) {
    return jsonError(400, "empty_input", "input_text is required");
  }
  if (input.length > MAX_INPUT_CHARS) {
    return jsonError(413, "too_long", `Input exceeds ${MAX_INPUT_CHARS} chars`);
  }

  const contextBlock = (body.context_block ?? "").trim();
  if (contextBlock.length > MAX_CONTEXT_BLOCK_CHARS) {
    return jsonError(
      413,
      "context_too_large",
      `Context block exceeds ${MAX_CONTEXT_BLOCK_CHARS} chars`,
    );
  }

  const route = (body.route ?? "generic").toLowerCase() as Route;
  if (!ROUTES.includes(route)) {
    return jsonError(400, "bad_route", `Unknown route: ${route}`);
  }
  const systemPrompt = SYSTEM_PROMPTS[route];

  // ---- Quota ----
  const { data: quotaData, error: quotaErr } = await admin
    .rpc("consume_daily_quota", { p_user_id: userId });
  if (quotaErr) {
    console.error("consume_daily_quota failed", quotaErr);
    return jsonError(500, "quota_error", "Could not check quota");
  }
  const quota = (quotaData?.[0] ?? null) as QuotaRow | null;
  if (!quota) {
    return jsonError(500, "quota_error", "Quota check returned no row");
  }
  if (!quota.allowed) {
    // Daily cap reached (free tier or lapsed pro). The client renders
    // this as "Upgrade for ₹99/mo" and opens the Razorpay subscription
    // checkout. resets_at is the next midnight IST so the UI can show
    // "resets in N hours" for free users who'd rather wait than pay.
    return jsonError(429, "quota_exhausted",
      "Daily limit reached — upgrade for unlimited or wait until midnight IST", {
      quota: {
        used:                quota.used,
        limit:               quota.quota_limit,
        remaining:           0,
        plan_tier:           quota.plan_tier,
        subscription_active: quota.subscription_active,
        resets_at:           nextIstMidnightIso(),
      },
    });
  }

  // ---- Groq call ----
  const startedAt  = Date.now();
  const controller = new AbortController();
  const timer      = setTimeout(() => controller.abort(), LLM_TIMEOUT_MS);

  let enhanced  = "";
  let success   = false;
  let errorKind: string | null = null;

  try {
    const resp = await fetch(GROQ_URL, {
      method:  "POST",
      signal:  controller.signal,
      headers: {
        Authorization:  `Bearer ${groqKey}`,
        "Content-Type": "application/json",
      },
      body: JSON.stringify({
        model:       GROQ_MODEL,
        temperature: 0.2,
        max_tokens:  2048,
        messages: [
          { role: "system", content: systemPrompt },
          {
            role: "user",
            // Layer 4: prepend the client-built context block when
            // present. The block already includes its own
            // <context>...</context> wrapper and the
            // <repository_digest>...</repository_digest> payload, so
            // we splice it verbatim before <input>.
            content: contextBlock
              ? `${contextBlock}\n\n<input>${input}</input>`
              : `<input>${input}</input>`,
          },
        ],
      }),
    });

    if (!resp.ok) {
      errorKind = `groq_${resp.status}`;
      console.error("Groq non-2xx", resp.status, await resp.text());
    } else {
      const data = await resp.json();
      enhanced = (data?.choices?.[0]?.message?.content ?? "").trim();
      success  = enhanced.length > 0;
      if (!success) errorKind = "empty_response";
    }
  } catch (e) {
    errorKind = (e as Error).name === "AbortError" ? "timeout" : "fetch_error";
    console.error("Groq call failed", e);
  } finally {
    clearTimeout(timer);
  }

  const latencyMs = Date.now() - startedAt;

  // Fire-and-forget log. Never block the user on telemetry.
  admin.from("enhancements").insert({
    user_id:    userId,
    route,
    latency_ms: latencyMs,
    success,
    error_kind: errorKind,
  }).then(({ error }) => {
    if (error) console.error("enhancements insert failed", error);
  });

  // Pro / unlimited users have quota_limit = INT_MAX from the RPC, which
  // would render absurdly as "0 / 2147483647" on the client. Treat those
  // as null so the client knows to render "Pro · renews ..." instead.
  const isMetered = quota.plan_tier === "free_hosted";
  const quotaPayload = {
    used:                isMetered ? quota.used : 0,
    limit:               isMetered ? quota.quota_limit : null,
    remaining:           isMetered ? Math.max(0, quota.quota_limit - quota.used) : null,
    plan_tier:           quota.plan_tier,
    subscription_active: quota.subscription_active,
    resets_at:           isMetered ? nextIstMidnightIso() : null,
  };

  if (!success) {
    return jsonError(502, errorKind ?? "enhance_failed", "Upstream LLM failed", {
      quota: quotaPayload,
    });
  }

  return new Response(JSON.stringify({
    enhanced_text: enhanced,
    latency_ms:    latencyMs,
    quota:         quotaPayload,
  }), {
    status:  200,
    headers: { ...corsHeaders(), "Content-Type": "application/json" },
  });
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

/// ISO timestamp of the next midnight in Asia/Kolkata (IST, UTC+05:30).
/// The new consume_daily_quota RPC buckets usage by IST date, so the
/// client's "resets in N hours" countdown should target the same
/// boundary. Avoids dragging in a TZ library by doing the offset math
/// directly: IST is exactly UTC+5:30, no DST.
function nextIstMidnightIso(): string {
  const IST_OFFSET_MS = (5 * 60 + 30) * 60 * 1000;
  const nowUtcMs = Date.now();
  const nowIstMs = nowUtcMs + IST_OFFSET_MS;
  // Day boundary in IST = midnight in IST = (IST_ms rounded down to day) + 1 day
  const dayMs = 24 * 60 * 60 * 1000;
  const nextIstMidnightIstMs = Math.floor(nowIstMs / dayMs) * dayMs + dayMs;
  // Translate back to UTC
  const nextIstMidnightUtcMs = nextIstMidnightIstMs - IST_OFFSET_MS;
  return new Date(nextIstMidnightUtcMs).toISOString();
}

