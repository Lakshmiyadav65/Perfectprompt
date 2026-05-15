//! The heart of the new architecture. `pipeline::run` is the one
//! function the hotkey and question-card callers call. It runs:
//!
//!   Stage A — intake     (normalize, length gate, adversarial)
//!   Stage B — cache      (LRU lookup on fingerprint)
//!   Stage C — router     (domain + complexity + ambiguity → route)
//!   Stage D — LLM        (single call_llm, route-specific knobs)
//!   Stage E — validate   (strip preambles/fences, reject bad output)
//!   Stage F — deliver    (cache, trace log, return)
//!
//! Every stage except D is pure Rust. D is the single LLM call in the
//! architecture. All five short-circuit paths (TooShort, TooLong,
//! Adversarial, Decline, validator-rejected, LLM-error) round-trip the
//! user's original input as a non-error fallback so the caller can
//! paste-as-is.

use anyhow::Result;
use std::time::Instant;
use tauri::{AppHandle, Manager, Runtime};

use crate::enhance::{call_llm, load_prompt};
use crate::intake::{self, IntakeResult};
use crate::router::{self, RoutingDecision};
use crate::trace::{self, TraceRecord};
use crate::validate::{self, ValidationOutcome};
use crate::AppState;

/// Input envelope into the pipeline. The hotkey/clarify callers
/// assemble this from their capture step.
#[derive(Debug, Clone)]
pub struct PipelineInput {
    pub raw_input: String,
    pub active_app: String,
    pub context: Option<DeveloperContext>,
}

/// Optional project context surfaced when the active app is classified
/// as a developer environment. Step 9 will have `developer_enhance`
/// produce this instead of its old envelope-builder.
#[derive(Debug, Clone)]
pub struct DeveloperContext {
    pub project_name: String,
    pub project_summary: String,
}

#[derive(Debug, Clone)]
pub struct PipelineOutput {
    pub final_text: String,
    pub used_fallback: bool,
    /// Friendly tray message ("Kept original — input too vague"), or
    /// `None` when no fallback fired.
    pub fallback_reason: Option<String>,
    pub trace: TraceRecord,
}

// Per-route LLM knobs from the brief.
const CODE_PROMPT: &str = "code-enhancer.md";
const WRITING_PROMPT: &str = "writing-enhancer.md";
const GENERIC_PROMPT: &str = "generic-enhancer.md";
const CODE_MAX_TOKENS: u32 = 200;
const CODE_TEMPERATURE: f32 = 0.3;
const WRITING_MAX_TOKENS: u32 = 400;
const WRITING_TEMPERATURE: f32 = 0.6;
const GENERIC_MAX_TOKENS: u32 = 300;
const GENERIC_TEMPERATURE: f32 = 0.4;

pub async fn run<R: Runtime>(
    app: &AppHandle<R>,
    pi: PipelineInput,
) -> Result<PipelineOutput> {
    let started = Instant::now();
    let mut tr = base_trace(&pi.raw_input);

    // ── Stage A: Intake ───────────────────────────────────────────
    let (normalized, fingerprint) = match intake::run(
        &pi.raw_input,
        &pi.active_app,
        &intake::IntakeConfig::default(),
    ) {
        IntakeResult::Pass {
            normalized,
            fingerprint,
            ..
        } => (normalized, fingerprint),
        IntakeResult::TooShort => {
            tr.route = "intake_too_short".into();
            tr.reject_reason = Some("input too short".into());
            return Ok(finalize_fallback(app, tr, &pi.raw_input, started));
        }
        IntakeResult::TooLong => {
            tr.route = "intake_too_long".into();
            tr.reject_reason = Some("input too long".into());
            return Ok(finalize_fallback(app, tr, &pi.raw_input, started));
        }
        IntakeResult::Adversarial { pattern_name } => {
            tr.route = "intake_adversarial".into();
            tr.reject_reason = Some(format!("adversarial:{pattern_name}"));
            return Ok(finalize_fallback(app, tr, &pi.raw_input, started));
        }
    };

    // ── Stage B: Cache ────────────────────────────────────────────
    let cache = &app.state::<AppState>().cache;
    if let Some(cached) = cache.get(&fingerprint) {
        tr.route = "cache_hit".into();
        tr.cache_hit = true;
        tr.final_pasted_output = cached.clone();
        tr.total_latency_ms = started.elapsed().as_millis() as u64;
        trace::append(app, &tr);
        return Ok(PipelineOutput {
            final_text: cached,
            used_fallback: false,
            fallback_reason: None,
            trace: tr,
        });
    }

    // ── Stage C: Router ───────────────────────────────────────────
    let router_out = router::run(&normalized);
    tr.domain = Some(format!("{:?}", router_out.domain));
    tr.complexity = Some(router_out.complexity);
    tr.ambiguity = Some(router_out.ambiguity);

    let (prompt_file, max_tokens, temperature) = match &router_out.decision {
        RoutingDecision::Decline { reason } => {
            tr.route = "decline".into();
            tr.reject_reason = Some(reason.clone());
            return Ok(finalize_fallback(app, tr, &pi.raw_input, started));
        }
        RoutingDecision::Bypass => {
            // Input is already excellent — cache it as-is, deliver
            // unchanged. Not a fallback.
            cache.put(fingerprint.clone(), normalized.clone());
            tr.route = "bypass".into();
            tr.final_pasted_output = normalized.clone();
            tr.total_latency_ms = started.elapsed().as_millis() as u64;
            trace::append(app, &tr);
            return Ok(PipelineOutput {
                final_text: normalized,
                used_fallback: false,
                fallback_reason: None,
                trace: tr,
            });
        }
        RoutingDecision::Code => (CODE_PROMPT, CODE_MAX_TOKENS, CODE_TEMPERATURE),
        RoutingDecision::Writing => (WRITING_PROMPT, WRITING_MAX_TOKENS, WRITING_TEMPERATURE),
        RoutingDecision::Generic => (GENERIC_PROMPT, GENERIC_MAX_TOKENS, GENERIC_TEMPERATURE),
    };
    tr.route = route_label(&router_out.decision).into();

    // ── Stage D: LLM ──────────────────────────────────────────────
    let system_prompt = match load_prompt(app, prompt_file) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("[pipeline] failed to load prompt {prompt_file}: {e}");
            tr.reject_reason = Some("prompt load failed".into());
            return Ok(finalize_fallback(app, tr, &pi.raw_input, started));
        }
    };

    let user_message = build_user_message(&normalized, pi.context.as_ref());
    let llm_started = Instant::now();
    let llm_result =
        call_llm(app, &system_prompt, &user_message, max_tokens, temperature).await;
    tr.llm_called = true;
    tr.llm_latency_ms = Some(llm_started.elapsed().as_millis() as u64);

    let raw_output = match llm_result {
        Ok(o) => o,
        Err(e) => {
            eprintln!("[pipeline] LLM call failed: {e}");
            tr.reject_reason = Some(format!("llm_error: {e}"));
            return Ok(finalize_fallback(app, tr, &pi.raw_input, started));
        }
    };
    tr.raw_llm_output = Some(raw_output.clone());

    // ── Stage E: Validate ─────────────────────────────────────────
    // Per-route ratio caps. The default 8× from the validator's spec
    // was tuned against catastrophic explosions in the old single-
    // prompt architecture. The new Writing and Generic prompts
    // intentionally produce placeholder-rich specs that run 10–15×
    // the input, so a tight cap there kills legitimate outputs (see
    // trace 2026-05-14 #1 — 51-char LinkedIn input → 559-char clean
    // placeholder spec, ratio 11×, rejected). Code stays tight.
    let validator_cfg = match &router_out.decision {
        RoutingDecision::Writing => validate::ValidatorConfig {
            max_length_ratio: 20.0,
            min_output_chars: 20,
            min_input_chars_for_ratio: 5,
        },
        RoutingDecision::Generic => validate::ValidatorConfig {
            max_length_ratio: 15.0,
            min_output_chars: 15,
            min_input_chars_for_ratio: 5,
        },
        RoutingDecision::Code => validate::ValidatorConfig {
            // Short coding inputs that route to Code (not Decline) get
            // tightened to a 3-sentence rewrite — that's still ~10–15×
            // on a 20-char input. Decline at Stage C already catches
            // the catastrophic vague cases, so the validator's
            // defensive 8× cap is too tight here.
            max_length_ratio: 15.0,
            min_output_chars: 10,
            min_input_chars_for_ratio: 5,
        },
        _ => validate::ValidatorConfig::default(),
    };
    let validation = validate::validate_and_repair(&raw_output, &normalized, &validator_cfg);

    let (final_text, used_fallback) = match validation {
        ValidationOutcome::Repaired(s) => {
            tr.validation_outcome = "repaired".into();
            (s, false)
        }
        ValidationOutcome::Rejected(r) => {
            // Surface the exact reject reason on stderr so live tuning
            // doesn't require reading the JSONL trace every time.
            eprintln!("[validate] rejected: {r}");
            tr.validation_outcome = "rejected".into();
            tr.reject_reason = Some(r);
            (pi.raw_input.clone(), true)
        }
    };

    // ── Stage F: Cache + log + deliver ────────────────────────────
    if !used_fallback {
        cache.put(fingerprint, final_text.clone());
    }
    tr.final_pasted_output = final_text.clone();
    tr.total_latency_ms = started.elapsed().as_millis() as u64;
    let fallback_reason = if used_fallback {
        Some(friendly_reason(tr.reject_reason.as_deref().unwrap_or("")))
    } else {
        None
    };
    trace::append(app, &tr);

    Ok(PipelineOutput {
        final_text,
        used_fallback,
        fallback_reason,
        trace: tr,
    })
}

// ─────────────────────────────────────────────────────────────────────
// Helpers
// ─────────────────────────────────────────────────────────────────────

fn base_trace(raw_input: &str) -> TraceRecord {
    TraceRecord {
        ts_ms: trace::now_unix_millis(),
        raw_input: raw_input.to_string(),
        input_len: raw_input.chars().count(),
        route: String::new(),
        domain: None,
        complexity: None,
        ambiguity: None,
        cache_hit: false,
        llm_called: false,
        llm_latency_ms: None,
        raw_llm_output: None,
        final_pasted_output: raw_input.to_string(),
        validators_fired: vec![],
        validation_outcome: "n/a".into(),
        reject_reason: None,
        total_latency_ms: 0,
    }
}

fn build_user_message(normalized: &str, context: Option<&DeveloperContext>) -> String {
    let mut out = format!("<input>\n{normalized}\n</input>");
    if let Some(ctx) = context {
        out.push_str(&format!(
            "\n\n<context>\nProject: {}\nSummary: {}\n</context>",
            ctx.project_name, ctx.project_summary
        ));
    }
    out
}

fn route_label(d: &RoutingDecision) -> &'static str {
    match d {
        RoutingDecision::Code => "code",
        RoutingDecision::Writing => "writing",
        RoutingDecision::Generic => "generic",
        RoutingDecision::Bypass => "bypass",
        RoutingDecision::Decline { .. } => "decline",
    }
}

/// Finalize a fallback branch: stamp latency, log, and produce the
/// PipelineOutput. Used for every non-success exit (intake reject,
/// router decline, prompt-load fail, LLM error). Validator-reject is
/// handled inline so its post-Stage-E logic flows naturally.
fn finalize_fallback<R: Runtime>(
    app: &AppHandle<R>,
    mut tr: TraceRecord,
    raw_input: &str,
    started: Instant,
) -> PipelineOutput {
    tr.final_pasted_output = raw_input.to_string();
    tr.total_latency_ms = started.elapsed().as_millis() as u64;
    let fallback_reason = friendly_reason(tr.reject_reason.as_deref().unwrap_or(""));
    trace::append(app, &tr);
    PipelineOutput {
        final_text: raw_input.to_string(),
        used_fallback: true,
        fallback_reason: Some(fallback_reason),
        trace: tr,
    }
}

/// Map internal reason strings to human-friendly tray messages.
/// Public so Step 10 can reuse the same mapping if it grows a richer
/// notifier API; for now `pipeline::run` is the sole caller.
pub fn friendly_reason(internal: &str) -> String {
    if internal == "input too vague" {
        "Kept original — input too vague".into()
    } else if internal.starts_with("adversarial:") {
        "Kept original — risky pattern in input".into()
    } else if internal == "input too short" {
        "Kept original — input too short".into()
    } else if internal == "input too long" {
        "Kept original — input too long".into()
    } else if internal.starts_with("llm_error") {
        "Kept original — LLM call failed".into()
    } else if internal == "prompt load failed" {
        "Kept original — internal config error".into()
    } else {
        "Kept original — output didn't pass checks".into()
    }
}

// ─────────────────────────────────────────────────────────────────────
// Tests — focused on the pure pieces (user-message build + friendly
// reason mapping). End-to-end pipeline tests need a real AppHandle and
// are exercised at acceptance time via the 15-input eval.
// ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_message_wraps_input_in_tags() {
        let m = build_user_message("refactor X", None);
        assert!(m.contains("<input>\nrefactor X\n</input>"));
        assert!(!m.contains("<context>"));
    }

    #[test]
    fn user_message_appends_context_when_supplied() {
        let ctx = DeveloperContext {
            project_name: "PromptForge".into(),
            project_summary: "Tauri + React prompt enhancer".into(),
        };
        let m = build_user_message("refactor X", Some(&ctx));
        assert!(m.contains("<input>\nrefactor X\n</input>"));
        assert!(m.contains("<context>\nProject: PromptForge"));
        assert!(m.contains("Summary: Tauri + React prompt enhancer"));
        assert!(m.contains("</context>"));
    }

    #[test]
    fn friendly_reason_maps_input_too_vague() {
        assert_eq!(
            friendly_reason("input too vague"),
            "Kept original — input too vague"
        );
    }

    #[test]
    fn friendly_reason_maps_adversarial_with_pattern() {
        assert_eq!(
            friendly_reason("adversarial:ignore_above"),
            "Kept original — risky pattern in input"
        );
    }

    #[test]
    fn friendly_reason_defaults_to_validator_message() {
        assert_eq!(
            friendly_reason("output too long 12x input"),
            "Kept original — output didn't pass checks"
        );
    }
}
