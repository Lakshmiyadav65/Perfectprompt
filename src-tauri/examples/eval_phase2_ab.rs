//! Phase 2 â€” context-bleed A/B harness.
//!
//! Run: `cargo run --example eval_phase2_ab`
//!
//! Diagnostic harness for the OUTPUT 2 / OUTPUT 3 findings. Runs two
//! prompts against Groq under both context conditions:
//!
//!   - Prompt 1 (Favorites feature, ~1400 chars) â€” 3Ã— WITHOUT context,
//!     3Ã— WITH context. The triple-repeat exposes stochastic variance.
//!   - Prompt 2 (Bug report, ~1100 chars) â€” 1Ã— WITHOUT, 1Ã— WITH.
//!
//! The WITH passes use the same `<context>` block the production
//! `pipeline::run` would build for the currently-active project. The
//! WITHOUT passes drop the wrapper entirely, matching the no-active-
//! project user-message shape.
//!
//! Reads API key from `%APPDATA%/com.promptforge.app/settings.json`
//! (fallback to `$GROQ_API_KEY`). Reads the active project from
//! `%APPDATA%/com.promptforge.app/projects.json`. Reads any cached
//! GitHub fetch from `<app_config_dir>/project_cache/{id}.json`.
//!
//! Writes the report to `../docs/eval-phase2-ab-report-pre.md`
//! (relative to `src-tauri/Cargo.toml`). The `-pre` suffix is
//! intentional â€” a second pass after the validator + Mode B fixes
//! ship will write a sibling `-post` file for diffing.
//!
//! IMPORTANT: this harness reflects the code state at the time it
//! runs. It does not ship the validator fix or the Mode B NEVER
//! extension â€” those land between pass 1 (this) and pass 2 (the
//! `-post` report).

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use promptforge_lib::intake::{self, IntakeConfig, IntakeResult};
use promptforge_lib::pipeline;
use promptforge_lib::projects::{Project, ProjectStore};
use promptforge_lib::router::{self, RoutingDecision};
use promptforge_lib::validate::{self, ValidationOutcome, ValidatorConfig};

const GROQ_API_URL: &str = "https://api.groq.com/openai/v1/chat/completions";
const MODEL: &str = "llama-3.3-70b-versatile";
const HTTP_TIMEOUT_SECS: u64 = 60;
const ACTIVE_APP: &str = "ab-harness";

// â”€â”€â”€ Embedded fixtures â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

const PROMPT_1_LABEL: &str = "Prompt 1 â€” Favorites feature";
const PROMPT_1_INPUT: &str = r#"I want to add a "favorite prompts" feature. Users should be able to mark a prompt as a favorite from the recent-history view, and then access favorites from a new section in the sidebar. Favorites need to persist across app restarts.

Requirements:
- A star icon on each recent-history item; clicking it toggles favorite status
- The star is filled when the prompt is a favorite, outlined when not
- A new "Favorites" section in the sidebar between "Recent" and the usage card, collapsible
- Favorites limited to 50 max; trying to add a 51st shows a tray notification
- Removing a favorite from the Favorites section also unfavorites it everywhere
- Favorites persist on disk alongside other settings
- Favorites should sync to the trace log so they show up in eval reports
- Don't change the existing recent-history behavior, just add the favorite-marking on top
- Don't add cloud sync, multi-device sync, or sharing â€” local only
- Keep the visual style consistent with the existing sidebar

Tests should cover: marking/unmarking, the 50-limit, persistence across restarts, that recent-history filtering still works."#;

const PROMPT_1_5_LABEL: &str = "Prompt 1.5 — Refactor enhance.rs (Code-routed)";
const PROMPT_1_5_INPUT: &str = r#"Refactor the enhance.rs module to split its Groq HTTP client logic out of the route-specific enhancement logic. The HTTP client (request building, response parsing, error handling, API key reading) should move to its own module. The route-specific enhancement (loading the right system prompt per route, building the user message with <input> wrapping) should stay where pipeline.rs can call it.

Constraints:
- Don't change pipeline.rs's call signature. The function it currently calls should still exist with the same name and parameters.
- Don't change the question-card flow. Question generation has its own prompt and request shape; leave it alone.
- Keep existing tests passing. Add new tests for the split.
- Don't introduce a config struct or builder. Keep simple.

The Groq API key still comes from env vars at startup.

Suggest names for the two new modules and what goes in each."#;

const PROMPT_2_LABEL: &str = "Prompt 2 â€” Bug report";
const PROMPT_2_INPUT: &str = "Filing a bug. The enhancer is dropping output on inputs that contain code blocks.\n\nRepro: select this text in Notepad and hit Ctrl+Alt+E:\n\n  Here's my function:\n  \n```javascript\n  function debounce(fn, ms) {\n    let t;\n    return (...args) => {\n      clearTimeout(t);\n      t = setTimeout(() => fn(...args), ms);\n    };\n  }\n```\n  \n  Rewrite this to use TypeScript with proper types.\n\nExpected: the enhancer rewrites the request and preserves the code block.\n\nActual: the code block disappears from the output. The rewrite says \"Rewrite the function above\" but \"the function above\" is gone because the code block was stripped.\n\nI suspect Stage E's strip_code_fences validator is being too aggressive â€” it's stripping fences from inside the input as part of repair instead of only stripping fences the model wrapped around its output. Trace log for this run is in app_data_dir/traces/2025-05-14.jsonl, search for the entry where validators_fired contains strip_code_fences.\n\nFix should preserve fences that are clearly part of the user's input. Add a regression test that covers this exact case.";

// â”€â”€â”€ Wire types â€” match enhance::ChatRequest exactly â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

#[derive(Serialize)]
struct Message<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    max_tokens: u32,
    temperature: f32,
    messages: Vec<Message<'a>>,
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
}

#[derive(Deserialize)]
struct Choice {
    message: ResponseMessage,
}

#[derive(Deserialize)]
struct ResponseMessage {
    content: Option<String>,
}

#[derive(Deserialize)]
struct PersistedSettings {
    api_key: Option<String>,
}

// â”€â”€â”€ Per-route LLM knobs â€” keep in lock-step with pipeline.rs â”€â”€â”€â”€â”€â”€â”€â”€

struct RouteKnobs {
    prompt_file: &'static str,
    max_tokens: u32,
    temperature: f32,
    validator: ValidatorConfig,
}

fn knobs_for(route: &RoutingDecision) -> Option<RouteKnobs> {
    Some(match route {
        RoutingDecision::Code => RouteKnobs {
            prompt_file: "code-enhancer.md",
            max_tokens: 200,
            temperature: 0.3,
            validator: ValidatorConfig {
                max_length_ratio: 15.0,
                min_output_chars: 10,
                min_input_chars_for_ratio: 5,
                ..Default::default()
            },
        },
        RoutingDecision::Writing => RouteKnobs {
            prompt_file: "writing-enhancer.md",
            max_tokens: 400,
            temperature: 0.6,
            validator: ValidatorConfig {
                max_length_ratio: 20.0,
                min_output_chars: 20,
                min_input_chars_for_ratio: 5,
                ..Default::default()
            },
        },
        RoutingDecision::Generic => RouteKnobs {
            prompt_file: "generic-enhancer.md",
            max_tokens: 300,
            // Phase 2: lowered from 0.4 to 0.3 in lockstep with pipeline.rs.
            temperature: 0.3,
            validator: ValidatorConfig {
                max_length_ratio: 15.0,
                min_output_chars: 15,
                min_input_chars_for_ratio: 5,
                ..Default::default()
            },
        },
        _ => return None,
    })
}

// â”€â”€â”€ Run results â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

struct RunResult {
    input_len: usize,
    route: String,
    domain: Option<String>,
    ambiguity: Option<u32>,
    user_message_len: usize,
    /// Verbatim raw output from Groq (pre-validation).
    raw_output: Option<String>,
    /// Final text after validate_and_repair. On reject, this equals the
    /// original input (the fallback behaviour pipeline::run uses).
    final_output: String,
    raw_len: usize,
    final_len: usize,
    /// `Repaired (clean)` | `Repaired (string changed)` | `Rejected: â€¦`
    /// | `Fallback (intake/route): â€¦` | `Error: â€¦`.
    validators_fired: String,
    llm_called: bool,
    llm_latency_ms: u128,
    total_latency_ms: u128,
}

// â”€â”€â”€ Groq client with 429 backoff (copy of eval_pass2's pattern) â”€â”€â”€â”€â”€

fn parse_retry_after_from_body(body: &str) -> Option<Duration> {
    let re = regex::Regex::new(r"try again in ([\d.]+)\s*(ms|s)").ok()?;
    let caps = re.captures(body)?;
    let n: f64 = caps[1].parse().ok()?;
    let unit = caps.get(2)?.as_str();
    if unit == "ms" {
        Some(Duration::from_millis(n.ceil() as u64))
    } else {
        Some(Duration::from_secs_f64(n))
    }
}

async fn call_groq(
    client: &reqwest::Client,
    api_key: &str,
    system: &str,
    user: &str,
    max_tokens: u32,
    temperature: f32,
) -> Result<String, String> {
    const MAX_ATTEMPTS: u32 = 4;
    let body = ChatRequest {
        model: MODEL,
        max_tokens,
        temperature,
        messages: vec![
            Message { role: "system", content: system },
            Message { role: "user", content: user },
        ],
    };
    for attempt in 1..=MAX_ATTEMPTS {
        let resp = client
            .post(GROQ_API_URL)
            .bearer_auth(api_key)
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        let status = resp.status();
        if status.is_success() {
            let parsed: ChatResponse = resp.json().await.map_err(|e| e.to_string())?;
            return parsed
                .choices
                .into_iter()
                .next()
                .and_then(|c| c.message.content)
                .ok_or_else(|| "no content".to_string());
        }
        let body_str = resp.text().await.unwrap_or_default();
        if status.as_u16() == 429 && attempt < MAX_ATTEMPTS {
            let wait = parse_retry_after_from_body(&body_str)
                .unwrap_or(Duration::from_secs(30))
                + Duration::from_millis(500);
            let wait = wait.min(Duration::from_secs(70));
            eprintln!(
                "      [429] attempt {attempt}/{MAX_ATTEMPTS}, sleeping {}ms",
                wait.as_millis()
            );
            tokio::time::sleep(wait).await;
            continue;
        }
        return Err(format!("HTTP {status}: {body_str}"));
    }
    Err(format!("HTTP 429: exhausted {MAX_ATTEMPTS} attempts"))
}

// â”€â”€â”€ One A/B run â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

async fn run_one(
    input: &str,
    context_block: Option<&str>,
    client: &reqwest::Client,
    api_key: &str,
    prompts_dir: &Path,
) -> RunResult {
    let started = Instant::now();
    let input_len = input.chars().count();
    let context_present = context_block.is_some_and(|s| !s.trim().is_empty());

    let cfg = IntakeConfig::default();
    let normalized = match intake::run(input, ACTIVE_APP, &cfg) {
        IntakeResult::Pass { normalized, .. } => normalized,
        IntakeResult::TooShort => {
            return short_circuit(input, input_len, "intake_too_short", started);
        }
        IntakeResult::TooLong => {
            return short_circuit(input, input_len, "intake_too_long", started);
        }
        IntakeResult::Adversarial { pattern_name } => {
            return short_circuit(
                input,
                input_len,
                &format!("intake_adversarial:{pattern_name}"),
                started,
            );
        }
    };

    let r = router::run(&normalized, context_present);
    let domain = Some(format!("{:?}", r.domain));
    let ambiguity = Some(r.ambiguity);

    let knobs = match &r.decision {
        RoutingDecision::Decline { reason } => {
            return RunResult {
                input_len,
                route: "decline".into(),
                domain,
                ambiguity,
                user_message_len: 0,
                raw_output: None,
                final_output: input.to_string(),
                raw_len: 0,
                final_len: input_len,
                validators_fired: format!("Fallback (route decline): {reason}"),
                llm_called: false,
                llm_latency_ms: 0,
                total_latency_ms: started.elapsed().as_millis(),
            };
        }
        RoutingDecision::Bypass => {
            return RunResult {
                input_len,
                route: "bypass".into(),
                domain,
                ambiguity,
                user_message_len: 0,
                raw_output: None,
                final_output: normalized.clone(),
                raw_len: 0,
                final_len: normalized.chars().count(),
                validators_fired: "Bypass (no LLM call, no validator)".into(),
                llm_called: false,
                llm_latency_ms: 0,
                total_latency_ms: started.elapsed().as_millis(),
            };
        }
        d => match knobs_for(d) {
            Some(k) => k,
            None => unreachable!("non-Code/Writing/Generic should have been handled"),
        },
    };

    let prompt_path = prompts_dir.join(knobs.prompt_file);
    let system_prompt = match fs::read_to_string(&prompt_path) {
        Ok(s) => s,
        Err(e) => {
            return RunResult {
                input_len,
                route: route_label(&r.decision).into(),
                domain,
                ambiguity,
                user_message_len: 0,
                raw_output: None,
                final_output: input.to_string(),
                raw_len: 0,
                final_len: input_len,
                validators_fired: format!("Error: prompt load failed: {e}"),
                llm_called: false,
                llm_latency_ms: 0,
                total_latency_ms: started.elapsed().as_millis(),
            };
        }
    };

    // Build the user message â€” `<context>...` prefix only when present
    // and non-empty. Exact mirror of `pipeline::build_user_message`.
    let user_msg = match context_block {
        Some(b) if !b.trim().is_empty() => {
            format!("{b}\n\n<input>\n{normalized}\n</input>")
        }
        _ => format!("<input>\n{normalized}\n</input>"),
    };

    let llm_started = Instant::now();
    let raw = match call_groq(
        client,
        api_key,
        &system_prompt,
        &user_msg,
        knobs.max_tokens,
        knobs.temperature,
    )
    .await
    {
        Ok(r) => r,
        Err(e) => {
            return RunResult {
                input_len,
                route: route_label(&r.decision).into(),
                domain,
                ambiguity,
                user_message_len: user_msg.chars().count(),
                raw_output: None,
                final_output: input.to_string(),
                raw_len: 0,
                final_len: input_len,
                validators_fired: format!("Error: llm_error: {e}"),
                llm_called: true,
                llm_latency_ms: llm_started.elapsed().as_millis(),
                total_latency_ms: started.elapsed().as_millis(),
            };
        }
    };
    let llm_latency_ms = llm_started.elapsed().as_millis();

    let validation = validate::validate_and_repair(&raw, &normalized, &knobs.validator);
    let (final_output, validators_fired) = match validation {
        ValidationOutcome::Repaired(s) => {
            let raw_trim = raw.trim();
            let label = if s.trim() == raw_trim {
                "Repaired (clean, no repair needed)"
            } else {
                "Repaired (string was modified by repair phase)"
            };
            (s, label.to_string())
        }
        ValidationOutcome::Rejected(reason) => {
            // pipeline::run falls back to the original input on reject.
            // Mirror that here so final_output reflects what the user
            // would actually see pasted.
            (input.to_string(), format!("Rejected: {reason}"))
        }
    };
    let raw_len = raw.chars().count();
    let final_len = final_output.chars().count();

    RunResult {
        input_len,
        route: route_label(&r.decision).into(),
        domain,
        ambiguity,
        user_message_len: user_msg.chars().count(),
        raw_output: Some(raw),
        final_output,
        raw_len,
        final_len,
        validators_fired,
        llm_called: true,
        llm_latency_ms,
        total_latency_ms: started.elapsed().as_millis(),
    }
}

fn short_circuit(
    input: &str,
    input_len: usize,
    route: &str,
    started: Instant,
) -> RunResult {
    RunResult {
        input_len,
        route: route.to_string(),
        domain: None,
        ambiguity: None,
        user_message_len: 0,
        raw_output: None,
        final_output: input.to_string(),
        raw_len: 0,
        final_len: input_len,
        validators_fired: format!("Fallback (intake): {route}"),
        llm_called: false,
        llm_latency_ms: 0,
        total_latency_ms: started.elapsed().as_millis(),
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

// â”€â”€â”€ Loaders for the active project + cached repo + API key â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

fn app_config_dir() -> Result<PathBuf, String> {
    let appdata = std::env::var("APPDATA").map_err(|_| "APPDATA env var not set".to_string())?;
    Ok(PathBuf::from(appdata).join("com.promptforge.app"))
}

fn load_api_key() -> Result<String, String> {
    if let Ok(k) = std::env::var("GROQ_API_KEY") {
        if !k.trim().is_empty() {
            return Ok(k.trim().to_string());
        }
    }
    let path = app_config_dir()?.join("settings.json");
    let txt = fs::read_to_string(&path)
        .map_err(|e| format!("could not read {}: {e}", path.display()))?;
    let parsed: PersistedSettings =
        serde_json::from_str(&txt).map_err(|e| format!("settings.json parse: {e}"))?;
    parsed
        .api_key
        .filter(|k| !k.trim().is_empty())
        .map(|k| k.trim().to_string())
        .ok_or_else(|| "settings.json has no api_key".to_string())
}

fn load_active_project() -> Result<Project, String> {
    let path = app_config_dir()?.join("projects.json");
    let txt = fs::read_to_string(&path)
        .map_err(|e| format!("could not read {}: {e}", path.display()))?;
    let store: ProjectStore = serde_json::from_str(&txt)
        .map_err(|e| format!("projects.json parse: {e}"))?;
    let id = store
        .active_project_id
        .ok_or_else(|| "no active project set".to_string())?;
    store
        .projects
        .into_iter()
        .find(|p| p.id == id)
        .ok_or_else(|| format!("active_project_id {id} not found in store"))
}

// â”€â”€â”€ Report rendering â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

// ─── Pass-1 baseline (captured from the pre-fix run console output) ──
//
// Used by `render_diff_section` to show before/after. If you re-run
// pass-1 (`-pre` report regenerates) and the numbers shift, update
// these constants accordingly — the diff is only as honest as the
// baseline.
const PRE_PROMPT1_WITHOUT_LENS: &[usize] = &[647, 1240, 1105];
const PRE_PROMPT1_WITH_LENS: &[usize] = &[690, 749, 767];
const PRE_PROMPT2_WITHOUT_LEN: usize = 381;
const PRE_PROMPT2_WITH_LEN: usize = 524;
const PRE_PROMPT1_INPUT_LEN: usize = 1400;
const PRE_PROMPT2_INPUT_LEN: usize = 1107;
const PRE_VALIDATOR_FIRINGS: usize = 0;
const PRE_SQE_LEAKS: usize = 0;

/// PromptForge-specific names from the active project's description
/// that the LLM should NOT volunteer unless the input names them.
/// Each is a Mode-B context-bleed risk. Used by the diff section to
/// count post-fix leaks vs the pass-1 baseline (which was 0).
const SUBSYSTEM_NAMES: &[&str] = &[
    "Smart Question Engine",
    "Wispr Flow",
    "PromptForge",
];

fn mean_ratio(lens: &[usize], input_len: usize) -> f32 {
    if lens.is_empty() || input_len == 0 {
        return 0.0;
    }
    let sum: f32 = lens.iter().map(|&l| l as f32 / input_len as f32).sum();
    sum / lens.len() as f32
}

fn range(lens: &[usize]) -> usize {
    if lens.is_empty() {
        return 0;
    }
    lens.iter().max().copied().unwrap_or(0)
        - lens.iter().min().copied().unwrap_or(0)
}

/// Count occurrences of `needle` (case-insensitive) in `haystack` that
/// are NOT already in `excluded` (the input). Returns the number of
/// "leaks" — names the model volunteered that the user didn't mention.
fn count_unanchored_occurrences(haystack: &str, excluded: &str, needle: &str) -> usize {
    let hay = haystack.to_lowercase();
    let inp = excluded.to_lowercase();
    let n = needle.to_lowercase();
    let in_output = hay.matches(&n).count();
    let in_input = inp.matches(&n).count();
    in_output.saturating_sub(in_input)
}

fn render_diff_section(results: &[(String, RunResult)]) -> String {
    // ASCII-only filters — em-dash encoding mismatch between the
    // label format strings and the filter literals caused these
    // collectors to return empty in an earlier iteration. `starts_with("Prompt 1 ")`
    // matches "Prompt 1 — ..." but NOT "Prompt 1.5 — ..." because
    // the latter has a period instead of a space after the 1.
    let p1_without: Vec<&RunResult> = results
        .iter()
        .filter(|(l, _)| l.starts_with("Prompt 1 ") && l.contains("WITHOUT"))
        .map(|(_, r)| r)
        .collect();
    let p1_with: Vec<&RunResult> = results
        .iter()
        .filter(|(l, _)| l.starts_with("Prompt 1 ") && l.contains("WITH context"))
        .map(|(_, r)| r)
        .collect();
    let p1_5_without: Vec<&RunResult> = results
        .iter()
        .filter(|(l, _)| l.starts_with("Prompt 1.5 ") && l.contains("WITHOUT"))
        .map(|(_, r)| r)
        .collect();
    let p1_5_with: Vec<&RunResult> = results
        .iter()
        .filter(|(l, _)| l.starts_with("Prompt 1.5 ") && l.contains("WITH context"))
        .map(|(_, r)| r)
        .collect();
    let p2_without = results
        .iter()
        .find(|(l, _)| l.starts_with("Prompt 2") && l.contains("WITHOUT"))
        .map(|(_, r)| r);
    let p2_with = results
        .iter()
        .find(|(l, _)| l.starts_with("Prompt 2") && l.contains("WITH context"))
        .map(|(_, r)| r);

    let p1_without_lens: Vec<usize> = p1_without.iter().map(|r| r.final_len).collect();
    let p1_with_lens: Vec<usize> = p1_with.iter().map(|r| r.final_len).collect();

    let pre_p1_without_avg = mean_ratio(PRE_PROMPT1_WITHOUT_LENS, PRE_PROMPT1_INPUT_LEN);
    let pre_p1_with_avg = mean_ratio(PRE_PROMPT1_WITH_LENS, PRE_PROMPT1_INPUT_LEN);
    let pre_p1_without_range = range(PRE_PROMPT1_WITHOUT_LENS);

    let post_p1_without_avg = if let Some(first) = p1_without.first() {
        mean_ratio(&p1_without_lens, first.input_len)
    } else {
        0.0
    };
    let post_p1_with_avg = if let Some(first) = p1_with.first() {
        mean_ratio(&p1_with_lens, first.input_len)
    } else {
        0.0
    };
    let post_p1_without_range = range(&p1_without_lens);

    // Mode A signal — count Rust idioms vs "softening" hits across
    // Prompt 1.5 WITH outputs (the Code-routed run where Mode A is
    // most testable).
    const RUST_IDIOMS: &[&str] =
        &["Result<", "Result::", "match ", " ?;", "?\n", " Tauri ", "invoke handler"];
    const SOFTENING: &[&str] = &[" consider ", " could ", " might ", "try/catch", " Promise", " async/await "];

    let mut p1_5_with_rust_hits = 0usize;
    let mut p1_5_with_soft_hits = 0usize;
    for r in &p1_5_with {
        for kw in RUST_IDIOMS {
            p1_5_with_rust_hits += r.final_output.matches(kw).count();
        }
        for kw in SOFTENING {
            p1_5_with_soft_hits += r.final_output.matches(kw).count();
        }
    }

    // Mode B signal — subsystem name leaks across all WITH outputs.
    let mut sqe_leaks_total = 0usize;
    let mut other_subsystem_leaks_total = 0usize;
    let with_iter = results.iter().filter(|(l, _)| l.contains("WITH context"));
    // ASCII-only prefix match (same em-dash workaround as above).
    // Order matters: "Prompt 1.5 " must be tested before "Prompt 1 "
    // so the longer prefix wins.
    let with_inputs_by_label: &[(&str, &str)] = &[
        ("Prompt 1.5 ", PROMPT_1_5_INPUT),
        ("Prompt 1 ", PROMPT_1_INPUT),
        ("Prompt 2 ", PROMPT_2_INPUT),
    ];
    for (label, r) in with_iter {
        let input = with_inputs_by_label
            .iter()
            .find(|(prefix, _)| label.starts_with(*prefix))
            .map(|(_, v)| *v)
            .unwrap_or("");
        for name in SUBSYSTEM_NAMES {
            let leaks = count_unanchored_occurrences(&r.final_output, input, name);
            if *name == "Smart Question Engine" {
                sqe_leaks_total += leaks;
            } else {
                other_subsystem_leaks_total += leaks;
            }
        }
    }

    // Validator firing count across all 14 outputs.
    let validator_firings = results
        .iter()
        .filter(|(_, r)| r.validators_fired.starts_with("Rejected:"))
        .count();

    // Build the markdown.
    let mut s = String::new();
    s.push_str("## Diff vs pass-1 baseline\n\n");
    s.push_str("Pass-1 values were captured from the pre-fix run's console output (see `eval-phase2-ab-report-pre.md`). Pass-2 values are computed live from this run.\n\n");
    s.push_str("| Metric | Pre (pass 1) | Post (pass 2) | Δ |\n|---|---:|---:|---:|\n");
    s.push_str(&format!(
        "| Prompt 1 WITHOUT avg ratio | {:.2} | {:.2} | {:+.2} |\n",
        pre_p1_without_avg,
        post_p1_without_avg,
        post_p1_without_avg - pre_p1_without_avg
    ));
    s.push_str(&format!(
        "| Prompt 1 WITHOUT char-range (max−min) | {} | {} | {:+} |\n",
        pre_p1_without_range,
        post_p1_without_range,
        post_p1_without_range as i64 - pre_p1_without_range as i64
    ));
    s.push_str(&format!(
        "| Prompt 1 WITH avg ratio | {:.2} | {:.2} | {:+.2} |\n",
        pre_p1_with_avg,
        post_p1_with_avg,
        post_p1_with_avg - pre_p1_with_avg
    ));
    let p1_5_route_label = p1_5_with
        .first()
        .map(|r| r.route.clone())
        .unwrap_or_else(|| "n/a".into());
    s.push_str(&format!(
        "| Prompt 1.5 route used | n/a (not run pre) | `{}` | (new) |\n",
        p1_5_route_label
    ));
    if let Some(p2w) = p2_without {
        let pre_ratio = PRE_PROMPT2_WITHOUT_LEN as f32 / PRE_PROMPT2_INPUT_LEN as f32;
        let post_ratio = p2w.final_len as f32 / p2w.input_len as f32;
        s.push_str(&format!(
            "| Prompt 2 WITHOUT ratio | {:.2} | {:.2} | {:+.2} |\n",
            pre_ratio,
            post_ratio,
            post_ratio - pre_ratio
        ));
    }
    if let Some(p2w) = p2_with {
        let pre_ratio = PRE_PROMPT2_WITH_LEN as f32 / PRE_PROMPT2_INPUT_LEN as f32;
        let post_ratio = p2w.final_len as f32 / p2w.input_len as f32;
        s.push_str(&format!(
            "| Prompt 2 WITH ratio | {:.2} | {:.2} | {:+.2} |\n",
            pre_ratio,
            post_ratio,
            post_ratio - pre_ratio
        ));
    }
    s.push_str(&format!(
        "| Validator firings (rejections, any of 14) | {} | {} | {:+} |\n",
        PRE_VALIDATOR_FIRINGS,
        validator_firings,
        validator_firings as i64 - PRE_VALIDATOR_FIRINGS as i64
    ));
    s.push_str(&format!(
        "| \"Smart Question Engine\" leaks across WITH outputs | {} | {} | {:+} |\n",
        PRE_SQE_LEAKS,
        sqe_leaks_total,
        sqe_leaks_total as i64 - PRE_SQE_LEAKS as i64
    ));
    s.push_str(&format!(
        "| Other subsystem-name leaks (PromptForge / Wispr Flow) | 0 | {} | {:+} |\n",
        other_subsystem_leaks_total, other_subsystem_leaks_total as i64
    ));
    s.push_str("\n");

    // Mode A scorecard — Prompt 1.5 WITH outputs specifically.
    s.push_str("### Mode A signal — Prompt 1.5 WITH outputs (the Code-routed test case)\n\n");
    s.push_str(&format!(
        "Counted across the {} WITH-context Prompt 1.5 outputs:\n\n",
        p1_5_with.len()
    ));
    s.push_str(&format!(
        "- Rust-idiom hits (`Result<`, `Result::`, `match `, ` ?;`, ` Tauri `, `invoke handler`): **{}**\n",
        p1_5_with_rust_hits
    ));
    s.push_str(&format!(
        "- Softening hits (` consider `, ` could `, ` might `, `try/catch`, ` Promise`, ` async/await `): **{}**\n",
        p1_5_with_soft_hits
    ));
    s.push_str("\n");
    if p1_5_with_rust_hits > 0 && p1_5_with_rust_hits >= p1_5_with_soft_hits {
        s.push_str("Verdict: Mode A is firing — Rust idioms appear at or above softening rate.\n\n");
    } else if p1_5_with_rust_hits == 0 {
        s.push_str("Verdict: Mode A is NOT firing — zero Rust idioms in Code-routed WITH outputs.\n\n");
    } else {
        s.push_str("Verdict: Mode A is weak — softening hits outnumber Rust idioms.\n\n");
    }

    // Prompt 1.5 WITHOUT/WITH numbers for completeness.
    let p1_5_without_lens: Vec<usize> = p1_5_without.iter().map(|r| r.final_len).collect();
    let p1_5_with_lens: Vec<usize> = p1_5_with.iter().map(|r| r.final_len).collect();
    let p1_5_input_len = p1_5_with.first().or(p1_5_without.first()).map(|r| r.input_len).unwrap_or(0);
    s.push_str("### Prompt 1.5 raw numbers (new in pass 2 — no pre-baseline to diff)\n\n");
    s.push_str(&format!(
        "- WITHOUT avg ratio: {:.2} (lengths {:?})\n",
        mean_ratio(&p1_5_without_lens, p1_5_input_len),
        p1_5_without_lens
    ));
    s.push_str(&format!(
        "- WITH avg ratio: {:.2} (lengths {:?})\n",
        mean_ratio(&p1_5_with_lens, p1_5_input_len),
        p1_5_with_lens
    ));
    s.push_str(&format!(
        "- WITHOUT char-range (variance): {}\n",
        range(&p1_5_without_lens)
    ));
    s.push_str(&format!(
        "- WITH char-range (variance): {}\n\n",
        range(&p1_5_with_lens)
    ));

    s
}

fn section(out: &mut String, header: &str, r: &RunResult) {
    out.push_str(&format!("### {header}\n\n"));
    out.push_str(&format!("- **Route:** `{}`", r.route));
    if let Some(d) = &r.domain {
        out.push_str(&format!(" Â· domain `{d}`"));
    }
    if let Some(a) = r.ambiguity {
        out.push_str(&format!(" Â· ambiguity `{a}`"));
    }
    out.push('\n');
    out.push_str(&format!(
        "- **Lengths:** input={} Â· user-message={} Â· raw-LLM={} Â· final={} (ratio {:.2})\n",
        r.input_len,
        r.user_message_len,
        r.raw_len,
        r.final_len,
        if r.input_len == 0 { 0.0 } else { r.final_len as f32 / r.input_len as f32 }
    ));
    out.push_str(&format!(
        "- **Latency:** total {}ms Â· LLM {}ms\n",
        r.total_latency_ms,
        if r.llm_called {
            r.llm_latency_ms.to_string()
        } else {
            "â€”".into()
        }
    ));
    out.push_str(&format!("- **Validators fired:** {}\n", r.validators_fired));
    out.push_str("\n**Final output:**\n\n");
    out.push_str("```\n");
    out.push_str(&r.final_output);
    out.push_str("\n```\n\n");
    if let Some(raw) = &r.raw_output {
        if raw.trim() != r.final_output.trim() {
            out.push_str("**Raw LLM output (before validator repair):**\n\n");
            out.push_str("```\n");
            out.push_str(raw);
            out.push_str("\n```\n\n");
        }
    }
    out.push_str("---\n\n");
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    println!("[ab] Phase 2 A/B harness â€” diagnostic pass 1 (pre-fixes)");

    let api_key = match load_api_key() {
        Ok(k) => k,
        Err(e) => {
            eprintln!("Could not load API key: {e}");
            std::process::exit(1);
        }
    };
    println!("[ab] API key loaded ({} chars)", api_key.len());

    let project = match load_active_project() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("Could not load active project: {e}");
            eprintln!("(set one via the ProjectManager UI before running the harness)");
            std::process::exit(1);
        }
    };
    println!(
        "[ab] active project: {} ({}, desc {} chars)",
        project.name,
        project.id,
        project.description.chars().count()
    );

    let cache_dir = match app_config_dir() {
        Ok(d) => d.join("project_cache"),
        Err(e) => {
            eprintln!("Could not resolve cache dir: {e}");
            std::process::exit(1);
        }
    };
    let cached = promptforge_lib::github_analyze::cached_repo(&cache_dir, &project.id);
    println!(
        "[ab] cached github fetch: {}",
        if cached.is_some() { "present" } else { "absent" }
    );

    // Build the exact <context> block the production pipeline would
    // build for this project. Same `assemble_context_block` the
    // orchestrator uses (modulo the path-scan, which this project
    // doesn't have since its `path` is null).
    let context_block =
        pipeline::assemble_context_block(Some(&project), None, cached.as_ref());
    let ctx_len = context_block.as_deref().map(|s| s.chars().count()).unwrap_or(0);
    println!(
        "[ab] context block: {} ({} chars)",
        if context_block.is_some() { "present" } else { "ABSENT" },
        ctx_len
    );
    if context_block.is_none() {
        eprintln!("Cannot run WITH passes â€” assemble_context_block returned None.");
        std::process::exit(1);
    }

    let manifest_dir: PathBuf = env!("CARGO_MANIFEST_DIR").into();
    let repo_root = manifest_dir.parent().expect("manifest has parent").to_path_buf();
    let prompts_dir = repo_root.join("prompts");
    let report_path = repo_root.join("docs").join("eval-phase2-ab-report-post.md");

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(HTTP_TIMEOUT_SECS))
        .build()
        .expect("http client");

    // ── Schedule the 14 runs ─────────────────────────────────────
    // Inter-call sleep keeps the harness under Groq's free-tier
    // tokens-per-minute budget. Each WITH-context call is ~2.5K
    // tokens (system prompt ~1.5K + context ~1.6K + input ~0.4K +
    // output ~0.7K); 12K TPM allows ~5 calls/min before throttling.
    // 12s sleep spreads 14 calls across ~3 minutes with margin.
    const INTER_CALL_SLEEP: Duration = Duration::from_secs(12);

    let mut results: Vec<(String, RunResult)> = Vec::new();

    // Prompt 1: 3 WITHOUT, 3 WITH.
    for i in 1..=3 {
        let label = format!("Prompt 1 â€” WITHOUT context, run {i}");
        println!("[ab] {label}");
        let r = run_one(PROMPT_1_INPUT, None, &client, &api_key, &prompts_dir).await;
        println!(
            "       route={} raw={} final={} llm={}ms",
            r.route, r.raw_len, r.final_len, r.llm_latency_ms
        );
        results.push((label, r));
        tokio::time::sleep(INTER_CALL_SLEEP).await;
    }
    for i in 1..=3 {
        let label = format!("Prompt 1 â€” WITH context, run {i}");
        println!("[ab] {label}");
        let r = run_one(
            PROMPT_1_INPUT,
            context_block.as_deref(),
            &client,
            &api_key,
            &prompts_dir,
        )
        .await;
        println!(
            "       route={} raw={} final={} llm={}ms",
            r.route, r.raw_len, r.final_len, r.llm_latency_ms
        );
        results.push((label, r));
        tokio::time::sleep(INTER_CALL_SLEEP).await;
    }
    // Prompt 1.5: 3 WITHOUT, 3 WITH — added Phase 2 to test Code-route Mode A
    // (Prompt 1 routed Generic in pass-1 dry-run; this prompt's
    // codebase-file names should reliably route to Code).
    for i in 1..=3 {
        let label = format!("Prompt 1.5 — WITHOUT context, run {i}");
        println!("[ab] {label}");
        let r = run_one(PROMPT_1_5_INPUT, None, &client, &api_key, &prompts_dir).await;
        println!(
            "       route={} raw={} final={} llm={}ms",
            r.route, r.raw_len, r.final_len, r.llm_latency_ms
        );
        results.push((label, r));
        tokio::time::sleep(INTER_CALL_SLEEP).await;
    }
    for i in 1..=3 {
        let label = format!("Prompt 1.5 — WITH context, run {i}");
        println!("[ab] {label}");
        let r = run_one(
            PROMPT_1_5_INPUT,
            context_block.as_deref(),
            &client,
            &api_key,
            &prompts_dir,
        )
        .await;
        println!(
            "       route={} raw={} final={} llm={}ms",
            r.route, r.raw_len, r.final_len, r.llm_latency_ms
        );
        results.push((label, r));
        tokio::time::sleep(INTER_CALL_SLEEP).await;
    }
    // Prompt 2: 1 WITHOUT, 1 WITH.
    {
        let label = "Prompt 2 â€” WITHOUT context".to_string();
        println!("[ab] {label}");
        let r = run_one(PROMPT_2_INPUT, None, &client, &api_key, &prompts_dir).await;
        println!(
            "       route={} raw={} final={} llm={}ms",
            r.route, r.raw_len, r.final_len, r.llm_latency_ms
        );
        results.push((label, r));
        tokio::time::sleep(INTER_CALL_SLEEP).await;
    }
    {
        let label = "Prompt 2 â€” WITH context".to_string();
        println!("[ab] {label}");
        let r = run_one(
            PROMPT_2_INPUT,
            context_block.as_deref(),
            &client,
            &api_key,
            &prompts_dir,
        )
        .await;
        println!(
            "       route={} raw={} final={} llm={}ms",
            r.route, r.raw_len, r.final_len, r.llm_latency_ms
        );
        results.push((label, r));
        tokio::time::sleep(INTER_CALL_SLEEP).await;
    }

    // â”€â”€ Render report â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
    let mut report = String::new();
    report.push_str("# Phase 2 A/B context-bleed diagnostic — pass 2 (post-fixes)\n\n");
    report.push_str(&format!(
        "Generated by `cargo run --example eval_phase2_ab`. Model: `{MODEL}`. \n\
         Code state: **after** the 4 Pass-2 fixes:\n\
         (1) tightened convention block in all 3 route prompts,\n\
         (2) extended Mode B NEVER to cover subsystem/feature names,\n\
         (3) `reject_if_outsources_content` validator shipped,\n\
         (4) Generic route temperature 0.4 → 0.3.\n\
         Pair the diff section at the end against `eval-phase2-ab-report-pre.md`.\n\n"
    ));
    report.push_str(&format!(
        "**Active project:** `{}` (id `{}`)  \n\
         **Project description length:** {} chars  \n\
         **Project path:** {}  \n\
         **Cached GitHub fetch:** {}  \n\
         **`<context>` block length:** {} chars  \n\
         **`CONTEXT_THRESHOLD_BUMP`:** 0 (routing unchanged WITH vs WITHOUT)\n\n",
        project.name,
        project.id,
        project.description.chars().count(),
        project.path.as_deref().unwrap_or("(none)"),
        if cached.is_some() { "present" } else { "absent" },
        ctx_len
    ));

    report.push_str("## Context block used for WITH passes (verbatim)\n\n");
    report.push_str("```\n");
    report.push_str(context_block.as_deref().unwrap_or(""));
    report.push_str("\n```\n\n---\n\n");

    report.push_str(&format!("## {PROMPT_1_LABEL} (~{} chars)\n\n", PROMPT_1_INPUT.chars().count()));
    for (label, r) in results.iter().filter(|(l, _)| l.starts_with("Prompt 1 ")) {
        let header = label.trim_start_matches("Prompt 1 — ");
        section(&mut report, header, r);
    }

    report.push_str(&format!(
        "## {PROMPT_1_5_LABEL} (~{} chars)\n\n",
        PROMPT_1_5_INPUT.chars().count()
    ));
    for (label, r) in results.iter().filter(|(l, _)| l.starts_with("Prompt 1.5 ")) {
        let header = label.trim_start_matches("Prompt 1.5 — ");
        section(&mut report, header, r);
    }

    report.push_str(&format!(
        "## {PROMPT_2_LABEL} (~{} chars)\n\n",
        PROMPT_2_INPUT.chars().count()
    ));
    for (label, r) in results.iter().filter(|(l, _)| l.starts_with("Prompt 2")) {
        let header = label.trim_start_matches("Prompt 2 — ");
        section(&mut report, header, r);
    }

    // Length-ratio summary table per prompt.
    for (prompt_tag, header_label) in [
        ("Prompt 1 ", "Prompt 1 — favorites"),
        ("Prompt 1.5 ", "Prompt 1.5 — refactor enhance.rs"),
        ("Prompt 2", "Prompt 2 — bug report"),
    ] {
        report.push_str(&format!(
            "## Length-ratio scorecard ({header_label}, threshold 0.33)\n\n"
        ));
        report.push_str("| Run | Input | Final | Ratio | Below 0.33? |\n|---|---|---|---|---|\n");
        for (label, r) in &results {
            if !label.starts_with(prompt_tag) {
                continue;
            }
            let ratio = if r.input_len == 0 {
                0.0
            } else {
                r.final_len as f32 / r.input_len as f32
            };
            let flag = if ratio < 0.33 { "**YES**" } else { "no" };
            let header = label
                .trim_start_matches(prompt_tag)
                .trim_start_matches("— ");
            report.push_str(&format!(
                "| {} | {} | {} | {:.2} | {} |\n",
                header, r.input_len, r.final_len, ratio, flag
            ));
        }
        report.push_str("\n");
    }

    // Diff section comparing pass-2 numbers against the pass-1
    // baseline. Pass-1 values are embedded here (captured from the
    // live console output of the pass-1 run). This is the proof
    // the fixes had measurable effect.
    report.push_str(&render_diff_section(&results));

    if let Err(e) = fs::create_dir_all(report_path.parent().unwrap()) {
        eprintln!("mkdir failed: {e}");
    }
    if let Err(e) = fs::write(&report_path, &report) {
        eprintln!("write failed: {e}");
        std::process::exit(1);
    }
    println!("\n[ab] wrote {}", report_path.display());
}
