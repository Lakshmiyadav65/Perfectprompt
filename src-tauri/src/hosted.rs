//! Hosted-tier enhancement call. The pipeline routes here (instead of
//! `enhance::call_llm`) when `auth::current_token()` returns Some — i.e.
//! the user has signed in via Supabase OAuth.
//!
//! Contract mirrors the Supabase edge function in
//! `supabase/functions/enhance/index.ts`:
//!   POST {SUPABASE_URL}/functions/v1/enhance
//!     Authorization: Bearer <user_jwt>
//!     body: { input_text, route }
//!     200 → { enhanced_text, latency_ms, quota }
//!     429 → { error: "quota_exhausted", quota }
//!     401 → token rejected (caller should clear session_token)
//!     502 → upstream Groq failed
//!
//! Known v1 limitation: project-context blocks are NOT sent on hosted
//! calls. The edge function wraps `input_text` in `<input>...</input>`
//! server-side, so prepending a `<context>` block here would produce
//! nested `<input>` tags. The Rust pipeline still computes the context
//! block (so trace.context_present is accurate), but hosted-path users
//! get the no-context variant of the prompt. Lift this restriction by
//! extending the edge function to accept a `context_block` field.

use std::time::Duration;

use serde::{Deserialize, Serialize};

const REQUEST_TIMEOUT_SECS: u64 = 30;

/// Mirrors the edge function's `quota` payload.
///
/// `limit` and `remaining` are nullable because pro / unlimited tiers
/// don't have a meaningful numeric ceiling under the daily-free +
/// monthly-pro model. The edge function emits `null` for those tiers;
/// metered (free_hosted) responses still carry real numbers. Display
/// code (the React hook) decides what to render based on plan_tier
/// and the presence of the numbers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostedQuota {
    pub used: u32,
    #[serde(default)]
    pub limit: Option<u32>,
    #[serde(default)]
    pub remaining: Option<u32>,
    pub plan_tier: String,
    #[serde(default)]
    pub subscription_active: Option<bool>,
    #[serde(default)]
    pub resets_at: Option<String>,
}

/// Typed result so the pipeline can surface tier-specific toasts
/// (quota exhausted, auth expired) distinctly from a generic fallback.
#[derive(Debug)]
pub enum HostedError {
    /// Returned 401 → token bad or expired. Caller should clear the
    /// stored session and ask the user to sign in again.
    Unauthorized,
    /// Returned 429 with quota body. Surface to the user with the
    /// "X/Y used today, resets at Z" toast.
    QuotaExhausted(HostedQuota),
    /// Network-level failure (DNS, TCP, TLS, timeout, body read).
    Network(String),
    /// Returned 2xx but body wasn't a parseable response.
    InvalidResponse(String),
    /// Anything else (400, 413, 500, 502, ...).
    Other { status: u16, body: String },
}

impl std::fmt::Display for HostedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HostedError::Unauthorized => write!(f, "hosted: token rejected (401)"),
            HostedError::QuotaExhausted(q) => {
                // QuotaExhausted only fires on the 429 path which is
                // always a metered (free_hosted) response, so `limit`
                // is always present — fall back to 0 just to satisfy
                // the formatter rather than panic on the unreachable.
                write!(
                    f,
                    "hosted: quota exhausted ({} / {})",
                    q.used,
                    q.limit.unwrap_or(0),
                )
            }
            HostedError::Network(e) => write!(f, "hosted network error: {e}"),
            HostedError::InvalidResponse(e) => write!(f, "hosted invalid response: {e}"),
            HostedError::Other { status, body } => {
                write!(f, "hosted http {status}: {}", trim(body, 200))
            }
        }
    }
}

impl std::error::Error for HostedError {}

fn trim(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut end = 0;
    for (i, _) in s.char_indices() {
        if i > max {
            break;
        }
        end = i;
    }
    format!("{}…", &s[..end])
}

/// Successful hosted enhancement. The pipeline takes `enhanced_text`
/// straight into Stage E validate, and emits `quota` to the frontend
/// so the Account section can update the daily counter live.
#[derive(Debug, Clone)]
pub struct HostedSuccess {
    pub enhanced_text: String,
    pub quota: HostedQuota,
}

#[derive(Serialize)]
struct HostedRequest<'a> {
    input_text: &'a str,
    route: &'a str,
}

#[derive(Deserialize)]
struct HostedOkBody {
    enhanced_text: String,
    quota: HostedQuota,
}

#[derive(Deserialize)]
#[allow(dead_code)] // error/message captured for future logging
struct HostedErrBody {
    error: Option<String>,
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    quota: Option<HostedQuota>,
}

/// POST to `{supabase_url}/functions/v1/enhance` with the user's JWT.
///
/// `route` must be one of `"code"`, `"writing"`, `"generic"` — matches
/// the edge function's `ROUTES` allow-list.
pub async fn call(
    supabase_url: &str,
    user_jwt: &str,
    input_text: &str,
    route: &str,
) -> Result<HostedSuccess, HostedError> {
    let url = format!(
        "{}/functions/v1/enhance",
        supabase_url.trim_end_matches('/')
    );

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS))
        .build()
        .map_err(|e| HostedError::Network(format!("client build: {e}")))?;

    let resp = client
        .post(&url)
        .bearer_auth(user_jwt)
        .json(&HostedRequest { input_text, route })
        .send()
        .await
        .map_err(|e| HostedError::Network(e.to_string()))?;

    let status = resp.status();
    let body = resp
        .text()
        .await
        .map_err(|e| HostedError::Network(format!("read body: {e}")))?;

    if status.is_success() {
        let parsed: HostedOkBody = serde_json::from_str(&body)
            .map_err(|e| HostedError::InvalidResponse(format!("parse ok body: {e}: {}", trim(&body, 200))))?;
        if parsed.enhanced_text.trim().is_empty() {
            return Err(HostedError::InvalidResponse("empty enhanced_text".into()));
        }
        return Ok(HostedSuccess {
            enhanced_text: parsed.enhanced_text,
            quota: parsed.quota,
        });
    }

    // Non-2xx path. Try to parse the typed error body so we can route
    // 401 / 429 distinctly even when the message text drifts.
    let parsed_err: Option<HostedErrBody> = serde_json::from_str(&body).ok();
    match status.as_u16() {
        401 => Err(HostedError::Unauthorized),
        429 => {
            if let Some(err) = parsed_err {
                if let Some(q) = err.quota {
                    return Err(HostedError::QuotaExhausted(q));
                }
            }
            Err(HostedError::Other { status: 429, body })
        }
        s => Err(HostedError::Other { status: s, body }),
    }
}

/// Compile-time fallback baked by `build.rs` from `.env` (or the build
/// env). Lets the installed `.exe` find Supabase even though no `.env`
/// is shipped alongside it. Empty string when neither the build env
/// nor the repo .env had the key — same shape `std::env::var` returns
/// for "unset".
const BAKED_SUPABASE_URL: &str = env!("SUPABASE_URL");

/// Resolve `SUPABASE_URL`, preferring a runtime env var (so dev workflows
/// can override the baked value) and falling back to the compile-time
/// constant. Without the fallback, the deployed binary has no `.env` to
/// read and the pipeline silently degrades to BYOK — which is why the
/// sidebar daily-quota counter never moved in production.
pub fn supabase_url() -> Option<String> {
    std::env::var("SUPABASE_URL")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| {
            let baked = BAKED_SUPABASE_URL.trim();
            if baked.is_empty() {
                None
            } else {
                Some(baked.to_string())
            }
        })
}
