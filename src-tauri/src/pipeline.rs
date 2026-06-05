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
use std::path::Path;
use std::sync::OnceLock;
use std::time::Instant;
use tauri::{AppHandle, Emitter, Manager, Runtime};

use regex::Regex;

use crate::auth;
use crate::enhance::{call_llm, load_prompt, GroqError};
use crate::enhancement_history;
use crate::github_analyze::{self, CachedRepo};
use crate::hosted::{self, HostedError};
use crate::intake::{self, IntakeResult};
use crate::project_scan::{self, ProjectSummary};
use crate::projects::{self, Project};
use crate::router::{self, RoutingDecision};
use crate::trace::{self, TraceRecord};
use crate::validate::{self, ValidationOutcome};
use crate::voice_diff::{self, DriftVerdict};
use crate::voice_fingerprint::{self, VoiceFingerprint};
use crate::AppState;

/// Input envelope into the pipeline. The hotkey/clarify callers
/// assemble this from their capture step. Project context is no longer
/// passed in by callers — `pipeline::run` resolves it from the active
/// project via `build_context_block` (Step 6).
#[derive(Debug, Clone)]
pub struct PipelineInput {
    pub raw_input: String,
    pub active_app: String,
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
// Phase 2 — Generic route dropped from 0.4 to 0.3 after the A/B pass-1
// dry-run showed 1.9× length variance on identical inputs (647/1240/1105
// chars). Generic is the catch-all for inputs without strong domain
// signal — exactly the inputs where the model has most latitude to
// diverge. Tighter sampling = more consistent rewrites. Code (0.3) and
// Writing (0.6, where tone variance is legitimate) are unchanged.
const GENERIC_TEMPERATURE: f32 = 0.3;

// Polish route knobs. Tight max_tokens because polished output is
// roughly the same length as the input; tight temperature because
// polish is a grammar-and-clarity task, not a creative one.
const POLISH_PROMPT: &str = "polish-enhancer.md";
const POLISH_MAX_TOKENS: u32 = 300;
const POLISH_TEMPERATURE: f32 = 0.2;

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

    // ── Phase 2 Step 6: project-context resolution ───────────────
    // Computed once per cache-miss invocation. Sits between Stage B and
    // Stage C so cache hits don't pay the (cheap) IO cost — and so the
    // router (Step 8) can read `context_present` for its Mode D
    // threshold relaxation. `effective_threshold` is set to the base
    // `DECLINE_THRESHOLD` here; Step 8 promotes it to 85 inside the
    // router when context is present.
    let active_project = projects::active_project_for(app);
    let context_block = build_context_block(app, active_project.as_ref());
    let context_present = context_block
        .as_deref()
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false);
    tr.context_present = context_present;

    // ── Stage C: Router ───────────────────────────────────────────
    let router_out = router::run(&normalized, context_present);
    tr.domain = Some(format!("{:?}", router_out.domain));
    tr.complexity = Some(router_out.complexity);
    tr.ambiguity = Some(router_out.ambiguity);
    tr.effective_threshold = router_out.effective_threshold;

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
    // Tier branch: signed-in users go through Supabase /enhance which
    // applies the system prompt server-side using HOSTED_GROQ_API_KEY
    // and enforces the lifetime free-trial cap via consume_lifetime_quota.
    // Signed-out users keep the BYOK path with their own Groq key — no
    // local quota gate since BYOK users pay Groq directly (per the
    // 2026-05-19 paywall decision: paywall applies to hosted only).
    let state = app.state::<AppState>();
    let token = auth::current_token(state.inner());
    let route_str = route_label(&router_out.decision);

    let llm_started = Instant::now();
    tr.llm_called = true;

    let raw_output = if let (Some(jwt), Some(supabase_url)) = (token, hosted::supabase_url()) {
        // Hosted path. Skips context_block in v1 — see hosted.rs
        // module docs for why. The frontend still sees the right
        // quota count via emitted event.
        let hosted_result = hosted::call(&supabase_url, &jwt, &normalized, route_str).await;
        tr.llm_latency_ms = Some(llm_started.elapsed().as_millis() as u64);
        match hosted_result {
            Ok(success) => {
                emit_quota_update(app, &success.quota);
                success.enhanced_text
            }
            Err(e) => {
                let (outcome, reason, friendly) = classify_hosted_error(&e);
                eprintln!("[pipeline] hosted call failed ({outcome}): {e}");
                tr.validation_outcome = outcome;
                tr.reject_reason = Some(reason);
                return Ok(finalize_fallback_with(
                    app,
                    tr,
                    &pi.raw_input,
                    started,
                    friendly,
                ));
            }
        }
    } else {
        // BYOK path (unchanged).
        let system_prompt = match load_prompt(app, prompt_file) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("[pipeline] failed to load prompt {prompt_file}: {e}");
                tr.reject_reason = Some("prompt load failed".into());
                return Ok(finalize_fallback(app, tr, &pi.raw_input, started));
            }
        };
        let user_message = build_user_message(&normalized, context_block.as_deref());
        let llm_result =
            call_llm(app, &system_prompt, &user_message, max_tokens, temperature).await;
        tr.llm_latency_ms = Some(llm_started.elapsed().as_millis() as u64);
        match llm_result {
            Ok(o) => o,
            Err(e) => {
                let (outcome, reason) = classify_llm_error(&e);
                eprintln!("[pipeline] LLM call failed ({outcome}): {e}");
                tr.validation_outcome = outcome;
                tr.reject_reason = Some(reason);
                return Ok(finalize_fallback(app, tr, &pi.raw_input, started));
            }
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
            ..Default::default()
        },
        RoutingDecision::Generic => validate::ValidatorConfig {
            max_length_ratio: 15.0,
            min_output_chars: 15,
            min_input_chars_for_ratio: 5,
            ..Default::default()
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
            ..Default::default()
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

    // Persist this run to the user-facing history if it's actually
    // an enhancement worth showing. Cache hits return earlier (their
    // record was created on the original enhancement), Decline and
    // intake-failures take the fallback path, and Bypass returns
    // earlier inside the routing match — so by the time we're here
    // and `!used_fallback`, the route is one of code/writing/generic
    // and the model produced something meaningful.
    //
    // Usage counting is authoritative on the server (consume_lifetime_quota
    // on the hosted path) — the frontend updates its display from the
    // `hosted:quota` event emitted by Stage D. BYOK users have no
    // quota counter at all.
    if !used_fallback
        && matches!(tr.route.as_str(), "code" | "writing" | "generic" | "polish")
    {
        enhancement_history::append(
            app,
            pi.raw_input.clone(),
            final_text.clone(),
            tr.route.clone(),
            active_project.as_ref().map(|p| p.id.clone()),
            active_project.as_ref().map(|p| p.name.clone()),
        );
    }

    Ok(PipelineOutput {
        final_text,
        used_fallback,
        fallback_reason,
        trace: tr,
    })
}

// ─────────────────────────────────────────────────────────────────────
// Polish pipeline — voice-preserving "clean up my message" path
// ─────────────────────────────────────────────────────────────────────

/// The polish pipeline. Mirrors [`run`] structurally but: (1) never
/// routes through the domain router (polish is a user-chosen mode, not
/// a classification), (2) fingerprints the input's voice and injects it
/// as an LLM constraint, and (3) re-fingerprints the output and rejects
/// if the model erased the user's voice (Stage 4). Triggered exclusively
/// by the capsule's Polish icon via `hotkey::trigger_polish`.
pub async fn run_polish<R: Runtime>(
    app: &AppHandle<R>,
    pi: PipelineInput,
) -> Result<PipelineOutput> {
    let started = Instant::now();
    let mut tr = base_trace(&pi.raw_input);

    // ── Stage A: Intake ───────────────────────────────────────────
    // Same length/adversarial gates as enhance, but the route label is
    // `polish_*` so trace readers can tell a polish-side intake reject
    // from an enhance-side one at a glance.
    let (normalized, raw_fp) = match intake::run(
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
            tr.route = "polish_too_short".into();
            tr.reject_reason = Some("input too short".into());
            return Ok(finalize_fallback(app, tr, &pi.raw_input, started));
        }
        IntakeResult::TooLong => {
            tr.route = "polish_too_long".into();
            tr.reject_reason = Some("input too long".into());
            return Ok(finalize_fallback(app, tr, &pi.raw_input, started));
        }
        IntakeResult::Adversarial { pattern_name } => {
            tr.route = "polish_adversarial".into();
            tr.reject_reason = Some(format!("adversarial:{pattern_name}"));
            return Ok(finalize_fallback(app, tr, &pi.raw_input, started));
        }
    };

    // Tag the route now so any failure from Stage B onwards traces as
    // "polish" rather than the empty default.
    tr.route = "polish".into();

    // ── Stage B: Cache (polish: namespace) ────────────────────────
    // The same EnhancementCache backs both pipelines. Prefix with
    // "polish:" so an enhance-cached fingerprint never returns a
    // polished output (or vice versa).
    let polish_key = format!("polish:{raw_fp}");
    let cache = &app.state::<AppState>().cache;
    if let Some(cached) = cache.get(&polish_key) {
        tr.route = "polish_cache_hit".into();
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

    // ── Project context ───────────────────────────────────────────
    // Polish injects the active project's <context> block so the
    // polished message stays consistent with the project's vocabulary,
    // technical terms, and proper nouns. The polish prompt is explicit
    // that voice rules dominate — context only provides vocabulary,
    // never a licence to rewrite into a different register.
    let active_project = projects::active_project_for(app);
    let context_block = build_context_block(app, active_project.as_ref());
    let context_present = context_block
        .as_deref()
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false);
    tr.context_present = context_present;

    // ── Stage 1: Fingerprint the input's voice ────────────────────
    // This single object is the source of truth for "voice" through
    // the rest of the pipeline. Stage 2 reads it to build the LLM
    // constraint block; Stage 4 re-fingerprints the output and diffs.
    let input_fp = voice_fingerprint::fingerprint(&normalized);

    // ── Stage 2: Build constraint-injected user message ───────────
    // The voice signature block sits OUTSIDE <input> so the LLM reads
    // it as guidance, not as data to rewrite. The context block (if
    // present) is prepended so the polished output uses the project's
    // vocabulary.
    let voice_block = voice_fingerprint::render_voice_signature(&input_fp);
    let user_message = match context_block.as_deref() {
        Some(ctx) if !ctx.trim().is_empty() => {
            format!("{ctx}\n\n{voice_block}\n\n<input>\n{normalized}\n</input>")
        }
        _ => format!("{voice_block}\n\n<input>\n{normalized}\n</input>"),
    };

    // ── Stage D: Single LLM call ──────────────────────────────────
    // Hosted (signed-in) routes through the Supabase edge function so
    // polish counts against the daily quota. BYOK uses the user's own
    // Groq key. Both apply POLISH_PROMPT / POLISH_MAX_TOKENS /
    // POLISH_TEMPERATURE.
    let state = app.state::<AppState>();
    let token = auth::current_token(state.inner());
    let llm_started = Instant::now();
    tr.llm_called = true;

    let raw_output = if let (Some(jwt), Some(supabase_url)) = (token, hosted::supabase_url()) {
        // Hosted path. The voice signature is bundled into input_text
        // (the prompt treats it as guidance, and Stage 4 catches any
        // drift).
        let hosted_input = format!("{voice_block}\n\n{normalized}");
        let hosted_result = hosted::call(&supabase_url, &jwt, &hosted_input, "polish").await;
        tr.llm_latency_ms = Some(llm_started.elapsed().as_millis() as u64);
        match hosted_result {
            Ok(success) => {
                emit_quota_update(app, &success.quota);
                success.enhanced_text
            }
            Err(e) => {
                let (outcome, reason, friendly) = classify_hosted_error(&e);
                eprintln!("[polish] hosted call failed ({outcome}): {e}");
                tr.validation_outcome = outcome;
                tr.reject_reason = Some(reason);
                return Ok(finalize_fallback_with(
                    app,
                    tr,
                    &pi.raw_input,
                    started,
                    friendly,
                ));
            }
        }
    } else {
        // BYOK path. The voice signature sits between the system prompt
        // and the <input> block as polish-enhancer.md expects.
        let system_prompt = match load_prompt(app, POLISH_PROMPT) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("[polish] failed to load prompt {POLISH_PROMPT}: {e}");
                tr.reject_reason = Some("prompt load failed".into());
                return Ok(finalize_fallback(app, tr, &pi.raw_input, started));
            }
        };
        let llm_result = call_llm(
            app,
            &system_prompt,
            &user_message,
            POLISH_MAX_TOKENS,
            POLISH_TEMPERATURE,
        )
        .await;
        tr.llm_latency_ms = Some(llm_started.elapsed().as_millis() as u64);
        match llm_result {
            Ok(o) => o,
            Err(e) => {
                let (outcome, reason) = classify_llm_error(&e);
                eprintln!("[polish] LLM call failed ({outcome}): {e}");
                tr.validation_outcome = outcome;
                tr.reject_reason = Some(reason);
                return Ok(finalize_fallback(app, tr, &pi.raw_input, started));
            }
        }
    };
    tr.raw_llm_output = Some(raw_output.clone());

    // ── Stage 4: Voice diff check ─────────────────────────────────
    // Re-fingerprint the LLM output and compare to the input. If the
    // LLM erased voice — REJECT and fall back to the user's original
    // text. This is the contract that makes polish voice-preserving
    // instead of formal-by-default.
    let output_fp = voice_fingerprint::fingerprint(&raw_output);
    match voice_diff::diff(&input_fp, &output_fp) {
        DriftVerdict::Reject(reason) => {
            eprintln!("[polish] rejected: {reason}");
            tr.validation_outcome = "voice_drift".into();
            tr.reject_reason = Some(reason);
            return Ok(finalize_fallback_with(
                app,
                tr,
                &pi.raw_input,
                started,
                "Kept original — polish would have changed your voice".into(),
            ));
        }
        DriftVerdict::Accept => {}
    }

    // ── Stage E: Length / sanity validator ────────────────────────
    // Polish output should be ≈ same length as input. The 3× cap is
    // generous — room for grammar repairs that legitimately expand
    // short cryptic inputs. min_output_chars=1 because polished output
    // can legitimately compress to a couple words.
    let validator_cfg = validate::ValidatorConfig {
        max_length_ratio: 3.0,
        min_output_chars: 1,
        min_input_chars_for_ratio: 3,
        ..Default::default()
    };
    let validation = validate::validate_and_repair(&raw_output, &normalized, &validator_cfg);
    let (mid_text, used_fallback) = match validation {
        ValidationOutcome::Repaired(s) => {
            tr.validation_outcome = "repaired".into();
            (s, false)
        }
        ValidationOutcome::Rejected(r) => {
            eprintln!("[polish] validator rejected: {r}");
            tr.validation_outcome = "rejected".into();
            tr.reject_reason = Some(r);
            (pi.raw_input.clone(), true)
        }
    };

    // ── Stage 5: Polish-specific structural cleanup ───────────────
    let final_text = if used_fallback {
        mid_text
    } else {
        polish_structural_cleanup(&mid_text, &input_fp)
    };

    // ── Stage F: Cache + log + deliver ────────────────────────────
    if !used_fallback {
        cache.put(polish_key, final_text.clone());
    }
    tr.final_pasted_output = final_text.clone();
    tr.total_latency_ms = started.elapsed().as_millis() as u64;
    let fallback_reason = if used_fallback {
        Some(friendly_reason(tr.reject_reason.as_deref().unwrap_or("")))
    } else {
        None
    };
    trace::append(app, &tr);

    // Polish history entries are project-independent (polish is for
    // personal comms, not project-bound prompts) so project_id and
    // project_name are always None. The route="polish" tag lets the
    // dashboard label them distinctly.
    if !used_fallback {
        enhancement_history::append(
            app,
            pi.raw_input.clone(),
            final_text.clone(),
            "polish".to_string(),
            None,
            None,
        );
    }

    Ok(PipelineOutput {
        final_text,
        used_fallback,
        fallback_reason,
        trace: tr,
    })
}

/// Stage 5 helper. Apply polish-specific structural strips beyond what
/// the generic validator already does. Pure function — no I/O. The
/// Stage 4 voice diff already rejects most of the failure modes that
/// motivate this cleanup; the strips below are defense-in-depth for
/// edge cases the fingerprinter undercounts (unusual greeting phrasing,
/// markdown the LLM injects despite the prompt).
fn polish_structural_cleanup(output: &str, input_fp: &VoiceFingerprint) -> String {
    let mut s = output.trim().to_string();

    // 1) Strip wrapping quotes/backticks (the validator strips fences
    //    but not straight quotes around the entire output).
    if s.chars().count() >= 2 {
        let first = s.chars().next();
        let last = s.chars().last();
        let wrapped = matches!(
            (first, last),
            (Some('"'), Some('"')) | (Some('\''), Some('\'')) | (Some('`'), Some('`'))
        );
        if wrapped {
            let mut chars: Vec<char> = s.chars().collect();
            chars.remove(0);
            chars.pop();
            s = chars.into_iter().collect::<String>().trim().to_string();
        }
    }

    // 2) Strip preambles. Common LLM tics: "Here is the polished
    //    version:", "Polished: ...", "Sure! ...", "Of course! ...".
    //    Cut at the first ':' or '\n', whichever comes first.
    let lower_head: String = s.chars().take(40).collect::<String>().to_lowercase();
    let preamble_prefixes = [
        "here is", "here's", "polished:", "output:", "sure!", "of course!",
    ];
    let starts_with_preamble = preamble_prefixes.iter().any(|p| lower_head.starts_with(p));
    if starts_with_preamble {
        if let Some(idx) = s.find(|c: char| c == ':' || c == '\n') {
            let after = &s[idx..];
            let skip_len = after.chars().next().map(|c| c.len_utf8()).unwrap_or(1);
            s = s[idx + skip_len..].trim().to_string();
        }
    }

    // 3) Strip an injected greeting when the input didn't have one.
    //    Stage 4 should have already rejected this; the strip here is
    //    belt-and-braces for fingerprinter misses.
    if !input_fp.has_greeting {
        let lc = s.to_lowercase();
        let greeting_starters = ["hi ", "hello ", "dear ", "hey ", "greetings"];
        if greeting_starters.iter().any(|g| lc.starts_with(g)) {
            if let Some(idx) = s.find(',') {
                s = s[idx + 1..].trim().to_string();
                // Re-capitalize the new first letter so the body still
                // reads as a sentence.
                if let Some(first) = s.chars().next() {
                    if first.is_ascii_lowercase() {
                        let mut chars: Vec<char> = s.chars().collect();
                        chars[0] = first.to_ascii_uppercase();
                        s = chars.into_iter().collect();
                    }
                }
            }
        }
    }

    // 4) Strip an injected signoff at the tail.
    if !input_fp.has_signoff {
        let signoff_tails = [
            ", regards", ", best regards", ", yours", ", sincerely", ", thanks",
        ];
        let lc = s.to_lowercase();
        for tail in &signoff_tails {
            if let Some(idx) = lc.rfind(tail) {
                if s.len().saturating_sub(idx) < 30 {
                    s = s[..idx].trim_end().to_string();
                    break;
                }
            }
        }
    }

    // 5) Strip markdown. Polish inputs are always plain text in v1, so
    //    any markdown in the output was injected by the LLM and should
    //    be stripped to keep the polished message looking like a real
    //    chat/email message.
    while s.starts_with('#') || s.starts_with(" #") {
        if let Some(nl) = s.find('\n') {
            s = s[nl + 1..].trim_start().to_string();
        } else {
            break;
        }
    }
    s = strip_markdown_emphasis(&s);

    s.trim().to_string()
}

/// Two-pass markdown emphasis strip: **bold** → bold, then *italic* →
/// italic. Bold first so a `**foo**` block isn't seen as two `*foo*`
/// italics on the first pass.
fn strip_markdown_emphasis(input: &str) -> String {
    static BOLD: OnceLock<Regex> = OnceLock::new();
    static ITALIC: OnceLock<Regex> = OnceLock::new();
    let bold =
        BOLD.get_or_init(|| Regex::new(r"\*\*([^*\n]+)\*\*").expect("static bold regex"));
    let italic =
        ITALIC.get_or_init(|| Regex::new(r"\*([^*\n]+)\*").expect("static italic regex"));
    let stripped_bold = bold.replace_all(input, "$1");
    italic.replace_all(&stripped_bold, "$1").into_owned()
}

// ─────────────────────────────────────────────────────────────────────
// Context bundle assembly (Phase 2 Step 4)
// ─────────────────────────────────────────────────────────────────────

/// Hard ceiling on the assembled `<context>` block — everything inside
/// (and including) the wrapper tags. Step 6 emits the wrapped string
/// directly into the user message; the ceiling guards the LLM call.
const CONTEXT_MAX_CHARS: usize = 2000;
const CONTEXT_OPEN: &str = "<context>\n";
const CONTEXT_CLOSE: &str = "\n</context>";
const TRUNC_MARKER: &str = "\n... [truncated]";
/// Minimum char budget to bother rendering a partially-truncated
/// section. Below this we drop the section entirely — a 10-char readme
/// excerpt followed by `... [truncated]` is more noise than signal.
const MIN_TRUNCATED_SECTION: usize = 30;

/// Orchestrator-facing context-block builder. Resolves the scan and
/// GitHub cache for `project` via `app`, then delegates to the pure
/// [`assemble_context_block`].
///
/// Returns `None` when `project` is `None`, OR when no field of the
/// assembled bundle has any content. The wrapper tags are emitted only
/// when there is *something* to wrap — the brief is explicit: "When
/// no active project, `<context>` is omitted entirely. Don't emit
/// empty context tags."
pub(crate) fn build_context_block<R: Runtime>(
    app: &AppHandle<R>,
    project: Option<&Project>,
) -> Option<String> {
    let project = project?;

    let scan = project
        .path
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .and_then(|p| project_scan::scan_project_summary(Path::new(p)));

    let cached = projects::cached_context_path(app)
        .ok()
        .and_then(|dir| github_analyze::cached_repo(&dir, &project.id));

    assemble_context_block(Some(project), scan.as_ref(), cached.as_ref())
}

/// Pure context-bundle assembler. All sources are explicit arguments,
/// no filesystem touch — Step 4's testable surface.
///
/// Field source priority (per the Phase 2 brief, §"Context source
/// priority"):
///
/// - `Stack`: user description (when it contains ≥2 stack keywords) →
///   scan output → empty.
/// - `Tooling`, `Conventions`, `file_layout`: scan only.
/// - `Description`: project.description, verbatim.
/// - `Readme`: scan first, then the GitHub cache fallback.
///
/// The cache currently exposes no structured Stack/Tooling/Conventions
/// fields (only repo name, description, default_branch, html_url), so
/// it can only contribute to the Readme section via its description
/// blob.
/// Pure assembler — exposed for the Step 10 evaluation harness
/// (`examples/eval_phase2_context.rs`) and any future inspector tooling
/// that needs to render a bundle from explicit inputs. Production code
/// should use [`build_context_block`] which resolves scan/cache from
/// the live `AppHandle` automatically.
pub fn assemble_context_block(
    project: Option<&Project>,
    scan: Option<&ProjectSummary>,
    cached: Option<&CachedRepo>,
) -> Option<String> {
    let project = project?;

    // ── Core (load-bearing) section. Never truncated. ──────────────
    let mut core = String::new();
    core.push_str(&format!("Project: {}\n", project.name));

    let stack = stack_from_description(&project.description).or_else(|| {
        scan.map(|s| s.stack.clone())
            .filter(|s| !s.is_empty())
    });
    if let Some(s) = stack {
        core.push_str(&format!("Stack: {s}\n"));
    }

    if let Some(s) = scan {
        if !s.tooling.is_empty() {
            core.push_str(&format!("Tooling: {}\n", s.tooling));
        }
        if !s.conventions.is_empty() {
            core.push_str(&format!("Conventions: {}\n", s.conventions));
        }
    }

    let desc = project.description.trim();
    if !desc.is_empty() {
        core.push_str("\nDescription:\n");
        core.push_str(desc);
        core.push('\n');
    }

    // ── Optional 1: file layout. Truncated second under pressure. ──
    let layout = match scan {
        Some(s) if !s.file_layout.is_empty() => {
            let mut l = String::from("\nFile layout:\n");
            for entry in &s.file_layout {
                l.push_str("- ");
                l.push_str(entry);
                l.push('\n');
            }
            l
        }
        _ => String::new(),
    };

    // ── Optional 2: readme excerpt. Truncated first under pressure. ─
    let readme_body = scan
        .map(|s| s.readme_excerpt.clone())
        .filter(|s| !s.is_empty())
        .or_else(|| {
            cached
                .map(|c| c.repo.description.clone())
                .filter(|s| !s.is_empty())
        });
    let readme = match readme_body {
        Some(b) => format!("\nReadme:\n{b}\n"),
        None => String::new(),
    };

    let interior = fit_context_to_budget(&core, &layout, &readme);
    let trimmed = interior.trim_end();
    if trimmed.is_empty() {
        return None;
    }
    Some(format!("{CONTEXT_OPEN}{trimmed}{CONTEXT_CLOSE}"))
}

/// Enforce the 2000-char ceiling on the assembled bundle. Truncation
/// priority (per the brief): readme first, then file layout. Stack/
/// tooling/conventions/description are load-bearing and never trimmed
/// unless the core section alone exceeds the budget.
fn fit_context_to_budget(core: &str, layout: &str, readme: &str) -> String {
    let wrapper_len = CONTEXT_OPEN.len() + CONTEXT_CLOSE.len();
    let budget = CONTEXT_MAX_CHARS.saturating_sub(wrapper_len);

    if core.len() + layout.len() + readme.len() <= budget {
        return format!("{core}{layout}{readme}");
    }

    // ── Truncate readme first.
    let core_layout_len = core.len() + layout.len();
    if core_layout_len + TRUNC_MARKER.len() + MIN_TRUNCATED_SECTION <= budget {
        let room = budget - core_layout_len - TRUNC_MARKER.len();
        let trunc = safe_char_truncate(readme, room);
        return format!("{core}{layout}{trunc}{TRUNC_MARKER}");
    }
    if core_layout_len <= budget {
        return format!("{core}{layout}");
    }

    // ── Readme dropped. Truncate layout.
    let core_len = core.len();
    if core_len + TRUNC_MARKER.len() + MIN_TRUNCATED_SECTION <= budget {
        let room = budget - core_len - TRUNC_MARKER.len();
        let trunc = safe_char_truncate(layout, room);
        return format!("{core}{trunc}{TRUNC_MARKER}");
    }
    if core_len <= budget {
        return core.to_string();
    }

    // ── Even core exceeds budget. Hard-cap with marker — load-bearing
    // content is trimmed only as a last resort. We log so anyone tuning
    // can spot the pathological case in stderr.
    eprintln!(
        "[context] core section exceeds {CONTEXT_MAX_CHARS}-char ceiling; truncating"
    );
    let target = budget.saturating_sub(TRUNC_MARKER.len());
    format!("{}{TRUNC_MARKER}", safe_char_truncate(core, target))
}

fn safe_char_truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

/// Inspect the user-authored description for stack indicators. If the
/// description names ≥2 known technologies, the bundle's `Stack` line
/// is populated from the description's matches rather than from the
/// scan — per the Phase 2 brief example where a user-written
/// description supersedes scan-derived stack data.
///
/// Returns `None` when the description has fewer than two recognised
/// keywords. Returns `Some("Rust, Tauri, React")` for descriptions
/// like "A Rust Tauri 2 + React 19 app." (Note: this is a normalised
/// list — the original description text still appears verbatim in the
/// bundle's `Description:` section, so version detail is preserved.)
fn stack_from_description(desc: &str) -> Option<String> {
    static KEYWORDS: OnceLock<Vec<(Regex, &'static str)>> = OnceLock::new();
    let kws = KEYWORDS.get_or_init(|| {
        let pairs: &[(&str, &str)] = &[
            (r"(?i)\brust\b", "Rust"),
            (r"(?i)\btauri\b", "Tauri"),
            (r"(?i)\breact\b", "React"),
            (r"(?i)\bvue(?:\.js)?\b", "Vue"),
            (r"(?i)\bsvelte\b", "Svelte"),
            (r"(?i)\bnext\.?js\b", "Next.js"),
            (r"(?i)\bvite\b", "Vite"),
            (r"(?i)\bpython\b", "Python"),
            (r"(?i)\bdjango\b", "Django"),
            (r"(?i)\bflask\b", "Flask"),
            (r"(?i)\btypescript\b", "TypeScript"),
            (r"(?i)\bnode\.?js\b", "Node.js"),
        ];
        pairs
            .iter()
            .map(|(p, l)| (Regex::new(p).expect("static stack regex"), *l))
            .collect()
    });

    let mut found: Vec<&'static str> = Vec::new();
    for (re, label) in kws.iter() {
        if re.is_match(desc) {
            found.push(label);
        }
    }
    if found.len() < 2 {
        None
    } else {
        Some(found.join(", "))
    }
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
        // Phase 2 Step 6: stays false / 0 until the orchestrator past
        // Stage B computes them. Short-circuited records keep the
        // defaults — readers learn the convention that a 0 threshold
        // means "no router decision was made for this record."
        context_present: false,
        effective_threshold: 0,
    }
}

/// Compose the LLM user message. The Phase 2 ordering is
/// `<context>...</context>\n\n<input>...</input>` — context first so the
/// model reads its grounding before the user's text. When no project is
/// active the `<context>` block is omitted entirely; we never emit
/// empty wrapper tags.
fn build_user_message(normalized: &str, context_block: Option<&str>) -> String {
    match context_block {
        Some(block) if !block.trim().is_empty() => {
            format!("{block}\n\n<input>\n{normalized}\n</input>")
        }
        _ => format!("<input>\n{normalized}\n</input>"),
    }
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
    if internal == "groq_rate_limit" {
        // Phase 2: surface rate-limit failures distinctly so users
        // know it's a Groq throttle (try again later) and not a
        // rewriting failure on their input. The wording is
        // intentionally vendor-agnostic — the in-app toast shouldn't
        // leak that we're using Groq specifically.
        "Your API limit has been reached.".into()
    } else if internal == "input too vague" {
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

/// Pure helper for Stage D's error-dispatch. Maps a `GroqError` to
/// the trace's `(validation_outcome, reject_reason)` pair. Rate-limit
/// errors get a distinct outcome string so they're greppable in
/// trace logs and the friendly_reason mapper can render the dedicated
/// "Groq API rate limit reached" toast.
///
/// Returns:
/// - `("fallback_rate_limit", "groq_rate_limit")` for `GroqError::RateLimit`
/// - `("n/a", "llm_error: …")` for every other variant (Network,
///   InvalidResponse, Other) — preserves the Phase 1 "n/a" semantics
///   that the trace reader expects when validation didn't run.
pub(crate) fn classify_llm_error(err: &GroqError) -> (String, String) {
    match err {
        GroqError::RateLimit { .. } => ("fallback_rate_limit".into(), "groq_rate_limit".into()),
        other => ("n/a".into(), format!("llm_error: {other}")),
    }
}

/// Map a `HostedError` to `(validation_outcome, reject_reason, toast)`.
/// The toast string is what we surface to the user; reject_reason stays
/// machine-greppable for trace logs.
pub(crate) fn classify_hosted_error(err: &HostedError) -> (String, String, String) {
    match err {
        HostedError::Unauthorized => (
            "fallback_auth_expired".into(),
            "hosted_unauthorized".into(),
            "Sign-in expired — please sign in again.".into(),
        ),
        HostedError::QuotaExhausted(q) => (
            "fallback_quota".into(),
            "hosted_quota_exhausted".into(),
            format!(
                "Daily limit reached ({}/{}) — open PerfectPrompt to upgrade or wait for midnight IST.",
                q.used,
                q.limit.unwrap_or(0),
            ),
        ),
        HostedError::Network(_) => (
            "n/a".into(),
            format!("hosted_network: {err}"),
            "Couldn't reach PerfectPrompt servers — try again or sign out to use your own key."
                .into(),
        ),
        HostedError::InvalidResponse(_) | HostedError::Other { .. } => (
            "n/a".into(),
            format!("hosted_error: {err}"),
            "Server error — kept original prompt.".into(),
        ),
    }
}

/// Finalize a fallback with an explicit user-facing reason string.
/// Used by the hosted-tier path where the toast text comes from
/// `classify_hosted_error` rather than the generic `friendly_reason`
/// map (which is keyed on BYOK-side reject reasons).
fn finalize_fallback_with<R: Runtime>(
    app: &AppHandle<R>,
    mut tr: TraceRecord,
    raw_input: &str,
    started: Instant,
    fallback_reason: String,
) -> PipelineOutput {
    tr.final_pasted_output = raw_input.to_string();
    tr.total_latency_ms = started.elapsed().as_millis() as u64;
    trace::append(app, &tr);
    PipelineOutput {
        final_text: raw_input.to_string(),
        used_fallback: true,
        fallback_reason: Some(fallback_reason),
        trace: tr,
    }
}

/// Push live quota numbers to the frontend so the Account section in
/// Settings can update without polling. Payload shape mirrors
/// `HostedQuota` (used / limit / remaining / plan_tier / resets_at).
fn emit_quota_update<R: Runtime>(app: &AppHandle<R>, quota: &hosted::HostedQuota) {
    if let Err(e) = app.emit("hosted:quota", quota) {
        eprintln!("[pipeline] emit hosted:quota failed: {e}");
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
    fn user_message_prepends_context_block_when_supplied() {
        let block = "<context>\nProject: Foo\nStack: Rust + Tauri\n</context>";
        let m = build_user_message("refactor X", Some(block));
        // Context must come first.
        assert!(m.starts_with("<context>\n"), "expected leading context: {m}");
        // Blank line separator between context and input.
        assert!(m.contains("</context>\n\n<input>\n"), "missing separator: {m}");
        assert!(m.ends_with("\n</input>"), "expected trailing input close: {m}");
        // The input body survives unchanged.
        assert!(m.contains("<input>\nrefactor X\n</input>"));
    }

    #[test]
    fn user_message_omits_empty_context_block() {
        // A blank/whitespace context block must not produce empty tags.
        let m = build_user_message("hello", Some("   \n\n   "));
        assert!(!m.contains("<context>"), "should not emit empty context: {m}");
        assert_eq!(m, "<input>\nhello\n</input>");
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

    // ── Phase 2: rate-limit dispatch ──────────────────────────────

    /// Brief Test 4: when call_llm returns RateLimit, the trace
    /// outcome is `fallback_rate_limit` and reject_reason is
    /// `groq_rate_limit`. Tested via the pure helper since the full
    /// pipeline::run requires a live AppHandle (Phase 1 §7.3).
    #[test]
    fn classify_llm_error_maps_rate_limit_to_dedicated_outcome() {
        let err = GroqError::RateLimit {
            message: "Rate limit reached...".into(),
        };
        let (outcome, reason) = classify_llm_error(&err);
        assert_eq!(outcome, "fallback_rate_limit");
        assert_eq!(reason, "groq_rate_limit");
    }

    /// Brief Test 5 (regression): non-rate-limit errors stay on the
    /// generic Fallback path. If this regresses, every Groq failure
    /// would surface the rate-limit toast — confusing and wrong.
    #[test]
    fn classify_llm_error_keeps_generic_outcome_for_non_rate_limit() {
        let err = GroqError::Network("connection refused".into());
        let (outcome, reason) = classify_llm_error(&err);
        assert_eq!(outcome, "n/a");
        assert!(
            reason.starts_with("llm_error:"),
            "non-rate-limit reasons should keep the llm_error: prefix: {reason}"
        );
        assert!(reason.contains("connection refused"));
        // Same regression check for Other (e.g. HTTP 500).
        let err500 = GroqError::Other {
            status: 500,
            body: "Internal server error".into(),
        };
        let (outcome500, reason500) = classify_llm_error(&err500);
        assert_eq!(outcome500, "n/a");
        assert!(reason500.starts_with("llm_error:"));
        // Same regression check for InvalidResponse.
        let err_bad = GroqError::InvalidResponse("no choices".into());
        let (outcome_bad, reason_bad) = classify_llm_error(&err_bad);
        assert_eq!(outcome_bad, "n/a");
        assert!(reason_bad.starts_with("llm_error:"));
    }

    /// The friendly_reason mapping is what the in-app Toast component
    /// ultimately renders. Pin the exact rate-limit body text so the
    /// "Your API limit has been reached." string can't drift without
    /// a deliberate code change.
    #[test]
    fn friendly_reason_maps_groq_rate_limit_to_exact_toast_text() {
        assert_eq!(
            friendly_reason("groq_rate_limit"),
            "Your API limit has been reached."
        );
    }

    /// Non-rate-limit `llm_error:` reasons still map to the generic
    /// "LLM call failed" body — regression guard so we don't
    /// accidentally collapse all LLM failures onto the rate-limit
    /// toast.
    #[test]
    fn friendly_reason_keeps_generic_llm_error_for_non_rate_limit_path() {
        let body = friendly_reason("llm_error: groq network error: connection refused");
        assert_eq!(body, "Kept original — LLM call failed");
    }

    // ── Step 4: context-block assembly ────────────────────────────

    fn fake_project(name: &str, desc: &str) -> Project {
        Project {
            id: format!("proj_{name}"),
            name: name.to_string(),
            description: desc.to_string(),
            links: vec![],
            path: None,
            created_at: "0s".into(),
            updated_at: "0s".into(),
        }
    }

    fn fake_summary(stack: &str, tooling: &str, conventions: &str) -> ProjectSummary {
        ProjectSummary {
            stack: stack.into(),
            tooling: tooling.into(),
            conventions: conventions.into(),
            file_layout: vec![],
            readme_excerpt: String::new(),
        }
    }

    fn fake_cached_readme(body: &str) -> CachedRepo {
        CachedRepo {
            repo: crate::github_analyze::AnalyzedRepo {
                name: "example".into(),
                description: body.into(),
                default_branch: "main".into(),
                html_url: "https://github.com/example/repo".into(),
            },
            fetched_at: "0s".into(),
        }
    }

    #[test]
    fn stack_from_description_finds_two_keywords() {
        let out = stack_from_description("A Rust Tauri 2 + React 19 app.");
        assert_eq!(out.as_deref(), Some("Rust, Tauri, React"));
    }

    #[test]
    fn stack_from_description_returns_none_below_threshold() {
        assert!(stack_from_description("Just a Rust thing.").is_none());
        assert!(stack_from_description("").is_none());
        assert!(stack_from_description("A nice description with no tech.").is_none());
    }

    #[test]
    fn stack_from_description_is_case_insensitive() {
        let out = stack_from_description("REACT and tauri together");
        assert_eq!(out.as_deref(), Some("Tauri, React"));
    }

    #[test]
    fn assemble_returns_none_without_project() {
        assert!(assemble_context_block(None, None, None).is_none());
    }

    #[test]
    fn assemble_emits_project_and_description_with_no_scan() {
        // Test 1 fixture: name="Foo", description with stack keywords,
        // no path, no scan. Expected:
        //   - wrapped in <context>...</context>
        //   - starts with "Project: Foo"
        //   - Stack: line derived from description
        //   - Description: section contains the verbatim text
        //   - under 2000 chars
        let p = fake_project("Foo", "Tauri 2 + React 19 app");
        let block = assemble_context_block(Some(&p), None, None).expect("block built");

        assert!(block.starts_with("<context>\n"), "block: {block}");
        assert!(block.ends_with("\n</context>"), "block: {block}");
        assert!(block.contains("Project: Foo"), "block: {block}");
        let stack_line = block
            .lines()
            .find(|l| l.starts_with("Stack:"))
            .expect("expected a Stack: line");
        assert!(stack_line.contains("Tauri"), "stack: {stack_line}");
        assert!(block.contains("\nDescription:\n"), "missing Description section");
        assert!(
            block.contains("Tauri 2 + React 19 app"),
            "description not verbatim: {block}"
        );
        assert!(block.len() <= CONTEXT_MAX_CHARS, "exceeded budget: {}", block.len());
    }

    #[test]
    fn assemble_uses_scan_stack_when_description_lacks_keywords() {
        let p = fake_project("Bar", "A short description.");
        let s = fake_summary("Rust + Tauri 2", "cargo, npm", "");
        let block = assemble_context_block(Some(&p), Some(&s), None).expect("block built");
        assert!(block.contains("Stack: Rust + Tauri 2"), "block: {block}");
        assert!(block.contains("Tooling: cargo, npm"), "block: {block}");
    }

    #[test]
    fn assemble_description_keyword_match_takes_precedence_over_scan() {
        let p = fake_project("Baz", "Rust Tauri 2 + React 19 + Vite 7");
        // Scan claims only Tauri + React; description wins for Stack.
        let s = fake_summary("Tauri + React", "cargo", "");
        let block = assemble_context_block(Some(&p), Some(&s), None).expect("block built");
        // Description-derived stack should appear, not the scan's narrower string.
        let stack_line = block
            .lines()
            .find(|l| l.starts_with("Stack:"))
            .expect("Stack line");
        assert!(stack_line.contains("Rust"), "description-stack should include Rust: {stack_line}");
        assert!(stack_line.contains("Vite"), "description-stack should include Vite: {stack_line}");
        // Description section preserves the original text (with versions).
        assert!(block.contains("Rust Tauri 2 + React 19 + Vite 7"));
    }

    #[test]
    fn assemble_includes_file_layout_when_scan_has_entries() {
        let p = fake_project("L", "");
        let mut s = fake_summary("", "", "");
        s.file_layout = vec!["src/".into(), "src/main.rs".into(), "Cargo.toml".into()];
        let block = assemble_context_block(Some(&p), Some(&s), None).expect("block built");
        assert!(block.contains("\nFile layout:\n"), "missing file layout section");
        assert!(block.contains("- src/"), "missing src/ entry");
        assert!(block.contains("- Cargo.toml"), "missing Cargo.toml entry");
    }

    #[test]
    fn assemble_uses_scan_readme_first() {
        let p = fake_project("R", "");
        let mut s = fake_summary("", "", "");
        s.readme_excerpt = "Local readme content.".into();
        let cached = fake_cached_readme("GitHub readme blob.");
        let block = assemble_context_block(Some(&p), Some(&s), Some(&cached)).expect("block built");
        assert!(block.contains("Local readme content."));
        assert!(!block.contains("GitHub readme blob."));
    }

    #[test]
    fn assemble_falls_back_to_cached_readme_when_scan_has_none() {
        let p = fake_project("R", "");
        let cached = fake_cached_readme("GitHub-only readme.");
        let block =
            assemble_context_block(Some(&p), None, Some(&cached)).expect("block built");
        assert!(block.contains("GitHub-only readme."));
    }

    #[test]
    fn fit_to_budget_no_truncation_when_under_cap() {
        let core = "core\n".to_string();
        let layout = "layout\n".to_string();
        let readme = "readme\n".to_string();
        let out = fit_context_to_budget(&core, &layout, &readme);
        assert_eq!(out, "core\nlayout\nreadme\n");
    }

    #[test]
    fn fit_to_budget_truncates_readme_first() {
        let core = "C".repeat(500);
        let layout = "L".repeat(500);
        // 1500 char readme — would overshoot the 2000 - wrapper budget.
        let readme = "R".repeat(1500);
        let out = fit_context_to_budget(&core, &layout, &readme);
        let wrap = CONTEXT_OPEN.len() + CONTEXT_CLOSE.len();
        assert!(out.len() + wrap <= CONTEXT_MAX_CHARS);
        assert!(out.contains(TRUNC_MARKER.trim()), "expected trunc marker: {out}");
        // core and layout intact:
        assert!(out.starts_with(&core));
        assert!(out.contains(&layout));
    }

    #[test]
    fn fit_to_budget_drops_readme_then_truncates_layout() {
        let core = "C".repeat(500);
        let layout = "L".repeat(2000); // bigger than budget alone
        let readme = "R".repeat(200);
        let out = fit_context_to_budget(&core, &layout, &readme);
        let wrap = CONTEXT_OPEN.len() + CONTEXT_CLOSE.len();
        assert!(out.len() + wrap <= CONTEXT_MAX_CHARS);
        // readme should be entirely absent.
        assert!(!out.contains("R"), "readme should be dropped: {out}");
        assert!(out.starts_with(&core));
        assert!(out.contains(TRUNC_MARKER.trim()));
    }

    #[test]
    fn fit_to_budget_hard_caps_core_when_core_alone_overflows() {
        let core = "X".repeat(3000);
        let out = fit_context_to_budget(&core, "", "");
        let wrap = CONTEXT_OPEN.len() + CONTEXT_CLOSE.len();
        assert!(
            out.len() + wrap <= CONTEXT_MAX_CHARS,
            "hard cap failed: {} chars",
            out.len()
        );
        assert!(out.ends_with(TRUNC_MARKER));
    }

    #[test]
    fn assemble_block_stays_under_budget_with_huge_readme() {
        let p = fake_project("Big", "Tauri + React app");
        let mut s = fake_summary("Rust + Tauri 2", "cargo, npm", "Jest co-located tests");
        s.readme_excerpt = "A".repeat(5000); // pathologically large
        let block = assemble_context_block(Some(&p), Some(&s), None).expect("block built");
        assert!(
            block.len() <= CONTEXT_MAX_CHARS,
            "expected <= {CONTEXT_MAX_CHARS}, got {}",
            block.len()
        );
        // Load-bearing content survived:
        assert!(block.contains("Project: Big"));
        assert!(block.contains("Stack:"));
        assert!(block.contains("Tooling: cargo, npm"));
        assert!(block.contains("Conventions: Jest co-located tests"));
    }
}
