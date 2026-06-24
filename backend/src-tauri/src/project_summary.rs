//! PROJECT.md generator — Project Knowledge architecture rethink.
//!
//! After five rounds of system-prompt tuning produced inconsistent
//! results (averages 2.7 / 2.0 / 3.8 / 3.2 / 2.6 across rounds 1–5),
//! the data showed full-digest injection had hit a reading-
//! comprehension ceiling: the upstream enhance LLM cannot reliably
//! scan 120 KB of context per call.
//!
//! This module replaces that with a one-shot LLM call at project
//! creation time that distils the digest into a curated ~2 KB
//! PROJECT.md. Per-enhance calls inject that markdown plus the
//! digest's `<directory_structure>` (a file-path index) — total
//! context ~10 KB, comfortably inside the LLM's high-recall window.
//!
//! This is the Cursor `.cursorrules` / Claude Code `CLAUDE.md` /
//! Continue.dev pattern, applied to the existing digester output.

use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, Runtime};

use crate::auth;
use crate::enhance::{call_llm, load_prompt, GroqError};
use crate::hosted::{self, HostedError};
use crate::repo_digest::RepoDigest;
use crate::AppState;

// ─────────────────────────────────────────────────────────────────────
// Tuning constants
// ─────────────────────────────────────────────────────────────────────

/// Hard cap on PROJECT.md length. The downstream code-enhancer prompt
/// assumes a 2 KB target with a 4 KB hard ceiling, so the LLM's
/// `<project_summary>` block stays comfortably inside the high-recall
/// region of the context window. Exceeding this hits the same "lost in
/// the middle" failure mode the rethink exists to escape.
pub const PROJECT_MD_MAX_BYTES: usize = 4_000;

/// Target length for the generator. Quoted in the system prompt so the
/// LLM aims for this. Acts as a soft guide; only `MAX_BYTES` is hard-
/// enforced via [`enforce_budget`].
pub const PROJECT_MD_TARGET_BYTES: usize = 2_500;

/// Bundled-prompt filename. Loaded via the existing `load_prompt`
/// path so dev + bundled installs both resolve it.
pub const GENERATOR_PROMPT_FILE: &str = "project-summary-generator.md";

/// Generator LLM knobs. Deterministic temperature so successive
/// Refresh clicks don't produce wildly different summaries —
/// reduces user confusion ("which one was right?") and makes
/// regression debugging tractable.
const GEN_MODEL_LABEL: &str = "llama-3.3-70b-versatile";
const GEN_MAX_TOKENS: u32 = 1_200;
const GEN_TEMPERATURE: f32 = 0.1;

/// Hosted-path route value. Must match the entry added to
/// `supabase/functions/enhance/_prompts.ts` and the ROUTES allow-list
/// in `index.ts`.
const HOSTED_ROUTE: &str = "project_summary";

/// Rate-limit retry budget. The generator is a ~30 KB-input call that
/// sits right at the Groq free-tier 30k-tokens-per-minute ceiling, so
/// a fresh "Add GitHub repo" started inside a rate-limit window will
/// 429 on its first attempt almost every time. Without backoff, the
/// user sees the digest succeed but PROJECT.md silently fail —
/// landing them in the heavy-fallback path that re-burns the limit
/// on every enhance.
///
/// 3 attempts with [30s, 60s] sleeps gives a total worst-case wait of
/// ~90s before giving up, which is the longest the per-minute window
/// could possibly remain locked. Tokio sleep is fine for our async
/// runtime; this code is never on the hot path of an enhance call.
const RATE_LIMIT_MAX_ATTEMPTS: u32 = 3;
const RATE_LIMIT_BACKOFFS_SECS: &[u64] = &[30, 60];

/// Fixed schema headers, in required order. The validator walks the
/// markdown and confirms every header appears exactly once and in
/// this sequence. If a header is missing or out of order, validation
/// fails — we'd rather error and let the user retry than silently
/// store malformed context.
pub const REQUIRED_HEADERS: &[&str] = &[
    "## What this is",
    "## Stack",
    "## Architecture",
    "## Existing capabilities",
    "## Conventions",
    "## Gotchas",
];

// ─────────────────────────────────────────────────────────────────────
// Top-level entry — called from digest_local_project /
// digest_github_project after the digester succeeds.
// ─────────────────────────────────────────────────────────────────────

/// Generate a PROJECT.md from a freshly-built digest. Single LLM call.
/// Returns the validated markdown string on success.
///
/// Validation strategy: call the LLM, validate, retry once with a
/// clarifying suffix if validation fails, surface the error if both
/// attempts fail. We do NOT silently store malformed markdown — the
/// caller (digest_local_project / digest_github_project) treats a
/// summary failure as "leave project_summary = None and let the
/// fallback path handle the next enhance call."
pub async fn generate_project_summary<R: Runtime>(
    app: &AppHandle<R>,
    digest: &RepoDigest,
) -> Result<String> {
    let system_prompt = load_prompt(app, GENERATOR_PROMPT_FILE)
        .with_context(|| format!("load {GENERATOR_PROMPT_FILE}"))?;

    // Compact-digest mode (Project Knowledge rethink, 2nd pass).
    //
    // The full digest can run 120-160 KB (~40K tokens) for medium
    // repos like tinygrad / got / express. Groq's free-tier llama
    // models cap at 30K tokens-per-minute INCLUDING input and
    // output, so a 40K-token generator call can never succeed on
    // free tier — it's larger than the per-minute budget in one
    // shot, and waits don't help.
    //
    // The generator prompt was already designed around three
    // signals: <api_surface>, <directory_structure>, and the
    // manifest. We extract just those (plus the README for
    // "What this is" text) — about 8-12 KB total — and send THAT
    // as the user message. PROJECT.md quality stays the same in
    // practice because those sections contain the information the
    // prompt asks the LLM to use; the bulk of the file-content
    // blocks the digest packs were always more useful for per-call
    // enhance (which now uses PROJECT.md + file_index anyway)
    // than for generator pre-processing.
    let compact = build_compact_digest_for_generator(&digest.digest_text);
    let user_message = format!("---\n{}", compact);

    let attempt = run_llm_call(app, &system_prompt, &user_message).await?;
    match validate_project_md(&attempt) {
        Ok(md) => return Ok(enforce_budget(&md)),
        Err(e) => eprintln!("[project-summary] first attempt invalid: {e}"),
    }

    // Retry once with a corrective suffix. Same system prompt; the
    // user message carries the rejection reason so the LLM has
    // concrete feedback rather than just "do it again".
    let retry_message = format!(
        "{user_message}\n\nYour previous response did not match the required schema. Re-emit following the EXACT six-section schema above, in the EXACT order, with no preamble."
    );
    let retry = run_llm_call(app, &system_prompt, &retry_message).await?;
    let md = validate_project_md(&retry)
        .map_err(|e| anyhow!("PROJECT.md validation failed after retry: {e}"))?;
    Ok(enforce_budget(&md))
}

/// Build a compact view of a digest suitable for the PROJECT.md
/// generator's input. Pulls just the high-signal sections:
///   - `<api_surface>` (file paths + exported symbols)
///   - `<directory_structure>` (file index)
///   - First `<file>` block (manifest — Cargo.toml / package.json /
///     pyproject.toml etc., since the digest is tier-sorted)
///   - Second `<file>` block (README, also tier-sorted to come
///     right after the manifest)
///
/// The full digest_text can be 120-160 KB (~40K tokens) for medium
/// repos. That exceeds Groq's free-tier 30k TPM in a single call so
/// the generator request itself gets 429'd and PROJECT.md never
/// lands — the user falls into the legacy-fallback path which then
/// 429s every subsequent enhance. This function fixes the root
/// cause by sending ~10 KB instead. Cap is enforced at 20 KB as a
/// belt-and-braces guard against pathologically large manifests.
pub(crate) fn build_compact_digest_for_generator(digest_text: &str) -> String {
    const COMPACT_MAX_BYTES: usize = 20_000;
    let mut out = String::with_capacity(16 * 1024);
    out.push_str("<digest_overview>\n");
    out.push_str(
        "The following are the high-signal sections of the project's digest. \
         Use them to fill in PROJECT.md.\n\n",
    );

    if let Some(s) = extract_section(digest_text, "<api_surface>", "</api_surface>") {
        out.push_str(&s);
        out.push_str("\n\n");
    }
    if let Some(s) =
        extract_section(digest_text, "<directory_structure>", "</directory_structure>")
    {
        out.push_str(&s);
        out.push_str("\n\n");
    }
    // First 2 file blocks = manifest + README (digest is tier-sorted).
    let mut cursor = 0usize;
    for _ in 0..2 {
        if let Some((block, next_cursor)) = next_file_block(digest_text, cursor) {
            if out.len() + block.len() > COMPACT_MAX_BYTES {
                break;
            }
            out.push_str(&block);
            out.push_str("\n\n");
            cursor = next_cursor;
        } else {
            break;
        }
    }

    out.push_str("</digest_overview>");
    if out.len() > COMPACT_MAX_BYTES {
        // Defensive — should be rare since each section is bounded
        // by the digester. Truncate hard at a UTF-8 char boundary.
        let mut end = COMPACT_MAX_BYTES;
        while end > 0 && !out.is_char_boundary(end) {
            end -= 1;
        }
        out.truncate(end);
        out.push_str("\n[truncated]\n</digest_overview>");
    }
    out
}

/// Extract a `<tag>...</tag>` slice from `text`. Returns the whole
/// slice including both tags. `None` when either marker is absent or
/// the close marker appears before the open one (malformed input).
fn extract_section(text: &str, open_tag: &str, close_tag: &str) -> Option<String> {
    let start = text.find(open_tag)?;
    let close = text[start..].find(close_tag)?;
    let end = start + close + close_tag.len();
    Some(text[start..end].to_string())
}

/// Find the next `<file path="...">...</file>` block starting at or
/// after `from`. Returns the block content and the cursor position
/// just past the closing tag, so callers can iterate forward without
/// re-matching.
fn next_file_block(text: &str, from: usize) -> Option<(String, usize)> {
    if from >= text.len() {
        return None;
    }
    let after_from = &text[from..];
    let rel_start = after_from.find("<file path=")?;
    let abs_start = from + rel_start;
    let rel_end = text[abs_start..].find("</file>")?;
    let abs_end = abs_start + rel_end + "</file>".len();
    Some((text[abs_start..abs_end].to_string(), abs_end))
}

/// Classified outcome of a single LLM attempt. Used so the retry
/// loop can pattern-match on whether the error is rate-limit (worth
/// retrying with backoff) vs everything else (giving up immediately).
enum AttemptOutcome {
    Ok(String),
    RateLimited(String),
    Fatal(anyhow::Error),
}

/// Dispatch the LLM call with rate-limit-aware retry. Hosted
/// (signed-in) path uses the new `project_summary` route on the
/// edge function; BYOK path uses the existing `call_llm` with the
/// bundled generator prompt. Either way the same system prompt +
/// user message contract applies.
///
/// On `GroqError::RateLimit` (BYOK) or `HostedError::QuotaExhausted`
/// (hosted), sleeps and retries up to [`RATE_LIMIT_MAX_ATTEMPTS`]
/// total. Other errors fail-fast.
async fn run_llm_call<R: Runtime>(
    app: &AppHandle<R>,
    system_prompt: &str,
    user_message: &str,
) -> Result<String> {
    let _ = (GEN_MODEL_LABEL, GEN_MAX_TOKENS); // anchor constants;
        // documented in the file header.

    let mut last_rate_limit_msg = String::new();
    for attempt in 1..=RATE_LIMIT_MAX_ATTEMPTS {
        match run_llm_call_once(app, system_prompt, user_message).await {
            AttemptOutcome::Ok(text) => return Ok(text),
            AttemptOutcome::RateLimited(msg) => {
                last_rate_limit_msg = msg.clone();
                if attempt as usize > RATE_LIMIT_BACKOFFS_SECS.len() {
                    // We've exhausted the backoff schedule.
                    break;
                }
                let sleep_secs = RATE_LIMIT_BACKOFFS_SECS[(attempt - 1) as usize];
                eprintln!(
                    "[project-summary] rate-limited on attempt {attempt}/{RATE_LIMIT_MAX_ATTEMPTS} ({msg}); sleeping {sleep_secs}s before retry"
                );
                tokio::time::sleep(Duration::from_secs(sleep_secs)).await;
            }
            AttemptOutcome::Fatal(e) => return Err(e),
        }
    }

    // All attempts exhausted while still rate-limited. Friendly
    // error so the caller surfaces it to the user instead of just
    // dropping silently into the legacy-fallback path.
    Err(anyhow!(
        "Groq rate limit did not clear after {} attempts (last: {}). \
         Wait a few minutes and click Refresh to retry — your project's \
         digest is already saved.",
        RATE_LIMIT_MAX_ATTEMPTS,
        last_rate_limit_msg,
    ))
}

/// Single LLM attempt. Returns a classified outcome so the caller's
/// retry loop can distinguish rate-limit (worth waiting + retrying)
/// from other errors (fail fast).
async fn run_llm_call_once<R: Runtime>(
    app: &AppHandle<R>,
    system_prompt: &str,
    user_message: &str,
) -> AttemptOutcome {
    let state = app.state::<AppState>();
    let token = auth::current_token(state.inner());

    if let (Some(jwt), Some(supabase_url)) = (token, hosted::supabase_url()) {
        // Hosted path. The edge function loads the generator prompt
        // server-side via SYSTEM_PROMPTS[route="project_summary"], so
        // we pass the user message as `input_text` and let the
        // function wrap it in <input> tags. The context_block field
        // is left None — this call is the GENERATOR, not an enhance
        // that consumes context.
        let timed = tokio::time::timeout(
            Duration::from_secs(45),
            hosted::call(&supabase_url, &jwt, user_message, HOSTED_ROUTE, None),
        )
        .await;
        let result = match timed {
            Err(_) => {
                return AttemptOutcome::Fatal(anyhow!(
                    "project-summary hosted call timed out"
                ));
            }
            Ok(r) => r,
        };
        match result {
            Ok(success) => AttemptOutcome::Ok(success.enhanced_text),
            // Quota-exhausted on the hosted free tier behaves the
            // same as a Groq 429 from the user's perspective: a
            // limit on how many calls fit in a window. Retry it.
            Err(HostedError::QuotaExhausted(q)) => AttemptOutcome::RateLimited(
                format!("hosted quota exhausted ({}/{})", q.used, q.limit.unwrap_or(0)),
            ),
            Err(e) => AttemptOutcome::Fatal(anyhow!(
                "project-summary hosted call failed: {e}"
            )),
        }
    } else {
        // BYOK path. Uses the user's Groq key via the existing
        // single-temp chat-completion path.
        let result = call_llm(
            app,
            system_prompt,
            user_message,
            GEN_MAX_TOKENS,
            GEN_TEMPERATURE,
        )
        .await;
        match result {
            Ok(text) => AttemptOutcome::Ok(text),
            Err(GroqError::RateLimit { message }) => {
                AttemptOutcome::RateLimited(format!("groq rate limit: {message}"))
            }
            Err(e) => AttemptOutcome::Fatal(anyhow!(
                "project-summary BYOK call failed: {e:#}"
            )),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────
// Schema validator
// ─────────────────────────────────────────────────────────────────────

/// Validate that `md` conforms to the PROJECT.md schema:
///   - Starts with an H1 (`# Name`)
///   - Contains every header in [`REQUIRED_HEADERS`] in the listed
///     order
///   - No header appears more than once
///   - Within byte budget after trimming
///
/// Returns the trimmed markdown on success or a descriptive error.
pub fn validate_project_md(md: &str) -> Result<String> {
    let trimmed = md.trim();
    if trimmed.is_empty() {
        return Err(anyhow!("PROJECT.md is empty"));
    }

    // First non-empty line must be an H1. Lets the project name pass
    // through to the LLM while keeping the structural contract.
    let first_line = trimmed
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("");
    if !first_line.starts_with("# ") {
        return Err(anyhow!(
            "PROJECT.md must start with an `# Project Name` H1 header, found {first_line:?}"
        ));
    }

    // Walk all H2 headers, verify they match REQUIRED_HEADERS in
    // order. Tolerate trailing whitespace on header lines.
    let h2_lines: Vec<&str> = trimmed
        .lines()
        .filter(|l| l.starts_with("## "))
        .map(|l| l.trim_end())
        .collect();

    if h2_lines.len() != REQUIRED_HEADERS.len() {
        return Err(anyhow!(
            "PROJECT.md must have exactly {} H2 sections, found {}: {:?}",
            REQUIRED_HEADERS.len(),
            h2_lines.len(),
            h2_lines
        ));
    }

    for (i, expected) in REQUIRED_HEADERS.iter().enumerate() {
        if h2_lines[i] != *expected {
            return Err(anyhow!(
                "PROJECT.md section {} should be {:?}, found {:?}",
                i + 1,
                expected,
                h2_lines[i]
            ));
        }
    }

    Ok(trimmed.to_string())
}

// ─────────────────────────────────────────────────────────────────────
// Budget enforcement
// ─────────────────────────────────────────────────────────────────────

/// Truncate a too-long PROJECT.md down to fit
/// [`PROJECT_MD_MAX_BYTES`]. Drops trailing sections first — Gotchas
/// is the most disposable (often empty anyway), then Conventions,
/// then trims Existing capabilities by removing bullets from the end.
/// The first four sections (What this is / Stack / Architecture /
/// Existing capabilities header itself) are never dropped — they're
/// load-bearing for the downstream LLM.
pub(crate) fn enforce_budget(md: &str) -> String {
    if md.len() <= PROJECT_MD_MAX_BYTES {
        return md.to_string();
    }

    let sections = split_sections(md);
    let mut out = sections.clone();

    // Try dropping Gotchas entirely first (last section).
    if let Some(idx) = find_section_index(&out, "## Gotchas") {
        out[idx].body.clear();
        if rejoin(&out).len() <= PROJECT_MD_MAX_BYTES {
            return rejoin(&out);
        }
    }

    // Still too long: drop Conventions body.
    if let Some(idx) = find_section_index(&out, "## Conventions") {
        out[idx].body.clear();
        if rejoin(&out).len() <= PROJECT_MD_MAX_BYTES {
            return rejoin(&out);
        }
    }

    // Last resort: trim the Existing capabilities body from the end
    // (line by line) until we fit. Keep at least the header.
    if let Some(idx) = find_section_index(&out, "## Existing capabilities") {
        // Collect owned strings so the loop can mutate `out` without
        // tripping the borrow checker.
        let lines: Vec<String> =
            out[idx].body.lines().map(|s| s.to_string()).collect();
        for keep in (0..lines.len()).rev() {
            let trimmed = lines[..keep].join("\n");
            out[idx].body = trimmed;
            if rejoin(&out).len() <= PROJECT_MD_MAX_BYTES {
                return rejoin(&out);
            }
        }
        out[idx].body.clear();
    }

    // Should never get here in practice — but truncate hard if the
    // first four sections still exceed budget.
    let rejoined = rejoin(&out);
    if rejoined.len() <= PROJECT_MD_MAX_BYTES {
        rejoined
    } else {
        let mut hard = rejoined;
        hard.truncate(PROJECT_MD_MAX_BYTES);
        hard
    }
}

#[derive(Debug, Clone)]
struct Section {
    header: String,
    body: String,
}

fn split_sections(md: &str) -> Vec<Section> {
    let mut sections: Vec<Section> = Vec::new();
    let mut current: Option<Section> = None;
    for line in md.lines() {
        if line.starts_with("# ") || line.starts_with("## ") {
            if let Some(s) = current.take() {
                sections.push(s);
            }
            current = Some(Section {
                header: line.to_string(),
                body: String::new(),
            });
        } else if let Some(s) = current.as_mut() {
            if !s.body.is_empty() {
                s.body.push('\n');
            }
            s.body.push_str(line);
        }
    }
    if let Some(s) = current.take() {
        sections.push(s);
    }
    sections
}

fn find_section_index(sections: &[Section], header: &str) -> Option<usize> {
    sections
        .iter()
        .position(|s| s.header.trim_end() == header)
}

fn rejoin(sections: &[Section]) -> String {
    let mut out = String::new();
    for s in sections {
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str(&s.header);
        if !s.body.is_empty() {
            out.push('\n');
            out.push_str(&s.body);
        }
    }
    out
}

// ─────────────────────────────────────────────────────────────────────
// Persisted shape
// ─────────────────────────────────────────────────────────────────────

/// Per-project summary record. Persisted on the `Project` struct in
/// projects.json. The `user_edited` flag lets the regenerator warn
/// before clobbering manual edits.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProjectSummary {
    /// Full validated markdown text. <=PROJECT_MD_MAX_BYTES bytes.
    pub markdown: String,
    /// ISO-8601 timestamp at generation (or last edit) time.
    pub generated_at: String,
    /// `true` after the user has saved a manual edit via the
    /// `update_project_summary` command. The regenerator should
    /// prompt for confirmation before overwriting an edited summary.
    pub user_edited: bool,
    /// Identifier of the model that produced this summary. Useful
    /// for debugging quality regressions across model upgrades and
    /// for the UI to surface "regenerated with a different model".
    pub generator_model: String,
}

// ─────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn well_formed_md() -> String {
        "# MyProject\n\n\
        ## What this is\n\
        It is a thing.\n\n\
        ## Stack\n\
        TypeScript + Node.\n\n\
        ## Architecture\n\
        - `src/` — source\n\n\
        ## Existing capabilities\n\
        - Logging\n\n\
        ## Conventions\n\
        - Tests use Jest\n\n\
        ## Gotchas\n\
        - None."
            .to_string()
    }

    #[test]
    fn validate_accepts_well_formed_markdown() {
        let md = well_formed_md();
        let v = validate_project_md(&md).expect("should accept");
        assert!(v.contains("## Existing capabilities"));
        assert!(v.contains("## Gotchas"));
    }

    #[test]
    fn validate_rejects_missing_section() {
        // Strip the `## Existing capabilities` header but leave the
        // bullet, so the rest of the structure looks plausible.
        let bad = well_formed_md().replace("## Existing capabilities\n", "");
        let err = validate_project_md(&bad).expect_err("should reject");
        let msg = err.to_string();
        assert!(
            msg.contains("Existing capabilities") || msg.contains("5"),
            "error should name the missing section: {msg}"
        );
    }

    #[test]
    fn validate_rejects_wrong_section_order() {
        // Swap Conventions and Existing capabilities.
        let bad = "# X\n\
        ## What this is\nA\n\
        ## Stack\nB\n\
        ## Architecture\nC\n\
        ## Conventions\nD\n\
        ## Existing capabilities\nE\n\
        ## Gotchas\nF\n";
        let err = validate_project_md(bad).expect_err("should reject");
        let msg = err.to_string();
        assert!(
            msg.contains("Existing capabilities") || msg.contains("Conventions"),
            "error should name the misordered section: {msg}"
        );
    }

    #[test]
    fn validate_rejects_missing_h1() {
        let bad = "## What this is\nA\n## Stack\nB\n## Architecture\nC\n\
        ## Existing capabilities\nD\n## Conventions\nE\n## Gotchas\nF\n";
        let err = validate_project_md(bad).expect_err("should reject");
        assert!(err.to_string().contains("H1"));
    }

    #[test]
    fn validate_rejects_empty_input() {
        assert!(validate_project_md("").is_err());
        assert!(validate_project_md("   \n\n   ").is_err());
    }

    #[test]
    fn enforce_budget_returns_input_when_under_cap() {
        let md = well_formed_md();
        let out = enforce_budget(&md);
        assert_eq!(out, md);
    }

    fn fake_full_digest() -> String {
        // Synthetic digest in the shape repo_digest::render_digest
        // emits. Used by the compact-view extractor tests.
        let mut s = String::new();
        s.push_str("<repository_digest source=\"github://x/y@main\" fetched_at=\"2026-01-01T00:00:00Z\" files_included=\"50\" files_elided=\"0\">\n");
        s.push_str("<api_surface>\n## File-path index\n### src/main.rs\n- fn main\n</api_surface>\n\n");
        s.push_str("<directory_structure>\nsrc/\n  main.rs\npackage.json\n</directory_structure>\n\n");
        s.push_str("<file path=\"package.json\">\n{\"name\":\"x\",\"version\":\"1.0.0\"}\n</file>\n\n");
        s.push_str("<file path=\"README.md\">\n# X\nThe X project.\n</file>\n\n");
        // A bunch of additional file blocks to simulate a large digest.
        for i in 0..40 {
            s.push_str(&format!(
                "<file path=\"src/lib/mod_{i}.rs\">\npub fn f_{i}() {{}}\n</file>\n\n"
            ));
        }
        s.push_str("</repository_digest>");
        s
    }

    #[test]
    fn compact_digest_includes_high_signal_sections() {
        let full = fake_full_digest();
        let compact = build_compact_digest_for_generator(&full);
        // All three priority sections present.
        assert!(compact.contains("<api_surface>"), "missing api_surface");
        assert!(
            compact.contains("<directory_structure>"),
            "missing directory_structure"
        );
        assert!(
            compact.contains("<file path=\"package.json\">"),
            "missing manifest file block"
        );
        assert!(
            compact.contains("<file path=\"README.md\">"),
            "missing README file block"
        );
        // Wrapper present.
        assert!(compact.starts_with("<digest_overview>"));
        assert!(compact.contains("</digest_overview>"));
    }

    #[test]
    fn compact_digest_drops_bulk_file_blocks() {
        let full = fake_full_digest();
        let compact = build_compact_digest_for_generator(&full);
        // None of the bulk synthetic blocks should appear — they
        // come after the manifest and README in the source.
        assert!(
            !compact.contains("mod_5.rs"),
            "compact view leaked bulk source block: still includes mod_5.rs"
        );
        assert!(
            !compact.contains("mod_20.rs"),
            "compact view leaked deep bulk source block"
        );
        // Resulting size is well under the per-minute budget concern.
        // The full digest is ~3 KB in the fixture but in production
        // would be 120+ KB; compact stays under 5 KB either way.
        assert!(
            compact.len() < 5_000,
            "compact view too large: {} bytes",
            compact.len()
        );
    }

    #[test]
    fn extract_section_returns_none_for_missing_tag() {
        assert!(extract_section("no tags here", "<x>", "</x>").is_none());
    }

    #[test]
    fn enforce_budget_drops_gotchas_first() {
        // Construct a markdown well over 4 KB so the budget enforcer
        // must drop trailing content. We deliberately put the
        // overflow in Gotchas + Conventions so dropping Gotchas
        // alone (Step 1 of the truncation cascade) brings us under.
        let gotchas_padding = "g".repeat(1_500);
        let convs_padding = "c".repeat(300);
        let caps_padding = "x".repeat(2_400);
        let mut md = String::from("# Big\n\n## What this is\nA\n\n");
        md.push_str("## Stack\nTypeScript.\n\n");
        md.push_str("## Architecture\n- `src/` — code\n\n");
        md.push_str(&format!("## Existing capabilities\n{caps_padding}\n\n"));
        md.push_str(&format!("## Conventions\n{convs_padding}\n\n"));
        md.push_str(&format!("## Gotchas\n{gotchas_padding}"));
        assert!(
            md.len() > PROJECT_MD_MAX_BYTES,
            "fixture must exceed budget for the test to be meaningful: {} <= {}",
            md.len(),
            PROJECT_MD_MAX_BYTES,
        );
        let out = enforce_budget(&md);
        assert!(
            out.len() <= PROJECT_MD_MAX_BYTES,
            "enforce_budget overshot: {}",
            out.len()
        );
        assert!(out.contains("## What this is"));
        assert!(out.contains("## Existing capabilities"));
        // Gotchas header may remain but its body should be gone.
        assert!(!out.contains(&"g".repeat(100)));
    }

    #[test]
    fn enforce_budget_drops_conventions_next_when_still_too_long() {
        // Make both Conventions and Gotchas oversized.
        let big = "y".repeat(2_500);
        let convs = "c".repeat(2_500);
        let mut md = String::from("# Big\n\n## What this is\nA\n\n");
        md.push_str("## Stack\nTypeScript.\n\n");
        md.push_str("## Architecture\n- `src/`\n\n");
        md.push_str("## Existing capabilities\n- Logging\n\n");
        md.push_str(&format!("## Conventions\n{convs}\n\n"));
        md.push_str(&format!("## Gotchas\n{big}"));
        assert!(md.len() > PROJECT_MD_MAX_BYTES);
        let out = enforce_budget(&md);
        assert!(out.len() <= PROJECT_MD_MAX_BYTES);
        // The four load-bearing sections survive.
        assert!(out.contains("## What this is"));
        assert!(out.contains("## Stack"));
        assert!(out.contains("## Architecture"));
        assert!(out.contains("## Existing capabilities"));
    }
}
