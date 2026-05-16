// PromptForge /enhance edge function.
//
// Auth: requires a Supabase user JWT (the Tauri client signs in via OAuth,
// then sends `Authorization: Bearer <access_token>`).
//
// Flow per request:
//   1. Verify JWT  → 401 if invalid.
//   2. Validate input (route, length cap).
//   3. consume_quota(user_id) — atomic increment-if-under-limit.
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
const ROUTES: readonly Route[] = ["code", "writing", "generic"];

interface QuotaRow {
  allowed: boolean;
  used: number;
  daily_limit: number;
  plan_tier: string;
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
  let body: { input_text?: string; route?: string };
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

  const route = (body.route ?? "generic").toLowerCase() as Route;
  if (!ROUTES.includes(route)) {
    return jsonError(400, "bad_route", `Unknown route: ${route}`);
  }
  const systemPrompt = SYSTEM_PROMPTS[route];

  // ---- Quota ----
  const { data: quotaData, error: quotaErr } = await admin
    .rpc("consume_quota", { p_user_id: userId });
  if (quotaErr) {
    console.error("consume_quota failed", quotaErr);
    return jsonError(500, "quota_error", "Could not check quota");
  }
  const quota = (quotaData?.[0] ?? null) as QuotaRow | null;
  if (!quota) {
    return jsonError(500, "quota_error", "Quota check returned no row");
  }
  if (!quota.allowed) {
    return jsonError(429, "quota_exhausted", "Daily limit reached", {
      quota: {
        used:        quota.used,
        limit:       quota.daily_limit,
        remaining:   0,
        plan_tier:   quota.plan_tier,
        resets_at:   nextUtcMidnightIso(),
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
          { role: "user",   content: `<input>${input}</input>` },
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

  if (!success) {
    return jsonError(502, errorKind ?? "enhance_failed", "Upstream LLM failed", {
      quota: {
        used:      quota.used,
        limit:     quota.daily_limit,
        remaining: Math.max(0, quota.daily_limit - quota.used),
        plan_tier: quota.plan_tier,
      },
    });
  }

  return new Response(JSON.stringify({
    enhanced_text: enhanced,
    latency_ms:    latencyMs,
    quota: {
      used:        quota.used,
      limit:       quota.daily_limit,
      remaining:   Math.max(0, quota.daily_limit - quota.used),
      plan_tier:   quota.plan_tier,
      resets_at:   nextUtcMidnightIso(),
    },
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

function nextUtcMidnightIso(): string {
  const now  = new Date();
  const next = new Date(Date.UTC(
    now.getUTCFullYear(),
    now.getUTCMonth(),
    now.getUTCDate() + 1,
    0, 0, 0, 0,
  ));
  return next.toISOString();
}
