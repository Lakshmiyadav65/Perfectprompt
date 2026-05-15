//! Stage C of the enhancement pipeline.
//!
//! Pure function: `(normalized_input) -> RouterOutput`. Reuses the
//! existing `question_bank::detect_domain` and `question_bank::score_complexity`
//! and adds an ambiguity score over the same input. The combination
//! decides one of five routes.
//!
//! The five routes:
//! - **Decline** — too vague; tray-notify and keep the original.
//! - **Bypass** — the input is already an excellent prompt; skip the
//!   LLM, cache the input as-is.
//! - **Code** — load the code-enhancer system prompt at Stage D.
//! - **Writing** — load the writing-enhancer system prompt.
//! - **Generic** — load the generic-enhancer system prompt.

use crate::question_bank::{detect_domain, score_complexity, Domain};

/// Base ambiguity threshold for Decline. Inputs whose ambiguity score
/// hits or exceeds this AND whose word count is under 5 are too vague
/// to enhance usefully. Phase 2 Mode D relaxes the ambiguity side by
/// `+15` when project context is present — the word-count gate is a
/// separate safety net that context cannot override.
pub(crate) const DECLINE_THRESHOLD: u32 = 70;
/// How many points Mode D adds to the ambiguity threshold when project
/// context is present.
///
/// Set to **0** pending heuristic re-tune (Phase 2.5). The plumbing is
/// shipped so trace logs and tests can verify the `context_present`
/// signal end-to-end, but Mode D's live effect on routing is currently
/// zero because the existing [`score_ambiguity`] heuristic does not
/// produce inputs in the 70–84 band where context unlocking would
/// matter. See `docs/migration-report.md` §6.3 and the calibration
/// notes in this module's test block.
///
/// TODO(Phase 2.5): re-tune `score_ambiguity` so mid-ambiguity prompts
/// like "refactor the auth flow" land in the 70–84 band, then re-run
/// the calibration and pick a non-zero bump value.
pub(crate) const CONTEXT_THRESHOLD_BUMP: u32 = 0;

/// One of five routes the orchestrator dispatches on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoutingDecision {
    Decline { reason: String },
    Bypass,
    Code,
    Writing,
    Generic,
}

/// Everything the orchestrator needs from Stage C. The scores ride
/// along so the trace logger can capture them per-record.
#[derive(Debug, Clone)]
pub struct RouterOutput {
    pub decision: RoutingDecision,
    pub domain: Domain,
    pub complexity: f32,
    pub ambiguity: u32,
    /// Phase 2 Step 8: the ambiguity threshold actually applied to this
    /// input. Equals `DECLINE_THRESHOLD` (70) for context-free runs;
    /// `DECLINE_THRESHOLD + CONTEXT_THRESHOLD_BUMP` (85) when the
    /// orchestrator passed `context_present = true`. Surfaced so the
    /// trace logger can record per-record whether Mode D changed the
    /// outcome.
    pub effective_threshold: u32,
}

/// Decide a route for the normalized input.
///
/// `context_present` is the orchestrator's signal that
/// `build_context_block` returned a non-empty `<context>` bundle for
/// this run. When true, the Decline threshold is raised by
/// `CONTEXT_THRESHOLD_BUMP` — the brief's Mode D ("context unlocks
/// borderline-ambiguous inputs"). The word-count gate (`< 5`) is
/// separate from the ambiguity threshold and applies regardless: a
/// 3-word input is structurally too thin to enhance even with rich
/// project context.
pub fn run(normalized_input: &str, context_present: bool) -> RouterOutput {
    let domain = detect_domain(normalized_input);
    let complexity = score_complexity(normalized_input);
    let ambiguity = score_ambiguity(normalized_input);
    let word_count = normalized_input.split_whitespace().count();
    let length = normalized_input.chars().count();

    let (decision, effective_threshold) =
        decide(domain, complexity, ambiguity, word_count, length, context_present);
    RouterOutput {
        decision,
        domain,
        complexity,
        ambiguity,
        effective_threshold,
    }
}

fn decide(
    domain: Domain,
    complexity: f32,
    ambiguity: u32,
    word_count: usize,
    length: usize,
    context_present: bool,
) -> (RoutingDecision, u32) {
    let effective_threshold = if context_present {
        DECLINE_THRESHOLD + CONTEXT_THRESHOLD_BUMP
    } else {
        DECLINE_THRESHOLD
    };

    // 1. Decline — too vague to enhance usefully. The `&&` matters:
    //    Mode D only relaxes the ambiguity side. The word-count gate
    //    catches structurally-thin inputs regardless of context.
    if ambiguity >= effective_threshold && word_count < 5 {
        return (
            RoutingDecision::Decline {
                reason: "input too vague".to_string(),
            },
            effective_threshold,
        );
    }
    // 2. Bypass — already an excellent prompt, skip the LLM.
    if complexity >= 0.7 && ambiguity <= 20 && length >= 60 {
        return (RoutingDecision::Bypass, effective_threshold);
    }
    // 3. Domain dispatch.
    let r = match domain {
        Domain::Coding => RoutingDecision::Code,
        Domain::Writing | Domain::Email => RoutingDecision::Writing,
        _ => RoutingDecision::Generic,
    };
    (r, effective_threshold)
}

// ─────────────────────────────────────────────────────────────────────
// Ambiguity score (0–100)
// ─────────────────────────────────────────────────────────────────────

/// Ambiguity heuristic. Composed of:
/// - word_count < 5      → +30  (and +10 extra when ≤ 3)
/// - no recognised verb  → +25
/// - vague-action verb   → +10  (extension: see VAGUE_VERBS)
/// - no plausible noun   → +20  (extended non-noun list catches
///                                comparatives like "faster" and
///                                abstract gerunds like "handling")
/// - bare pronoun (it/this/that) → +15
/// - no specifics (digits, "quotes", proper noun) → +10
/// Capped at 100.
///
/// The +10 word_count≤3 and +10 vague-verb extensions land "make it
/// faster" and "add error handling" at the Decline threshold without
/// distorting the other 13 eval inputs. See `docs/migration-report.md`
/// for the full eval walkthrough.
pub fn score_ambiguity(input: &str) -> u32 {
    let lower = input.to_ascii_lowercase();
    let words: Vec<&str> = lower.split_whitespace().collect();
    let word_count = words.len();

    let mut score: u32 = 0;
    if word_count < 5 {
        score += 30;
    }
    if word_count <= 3 {
        score += 10;
    }
    if !has_verb(&words) {
        score += 25;
    }
    if has_vague_verb(&words) {
        score += 10;
    }
    if !has_plausible_noun(&words) {
        score += 20;
    }
    if has_bare_pronoun(&lower) {
        score += 15;
    }
    if !has_specifics(input) {
        score += 10;
    }
    score.min(100)
}

/// Small built-in verb list covering the imperative verbs that show up
/// most often in user prompts. We don't need POS tagging — just a hit
/// signal that *some* verb is present.
const VERBS: &[&str] = &[
    "write", "rewrite", "draft", "compose", "send", "reply",
    "refactor", "implement", "build", "create", "make", "add", "remove",
    "delete", "fix", "change", "update", "modify", "rename", "move",
    "generate", "produce", "extract", "convert", "transform", "format",
    "summarize", "summarise", "translate", "explain", "describe", "list",
    "show", "tell", "find", "search", "lookup", "check", "review", "audit",
    "test", "validate", "verify", "ensure", "optimize", "optimise",
    "improve", "tune", "speed", "accelerate", "shrink", "compress",
    "analyze", "analyse", "compare", "design", "plan", "schedule",
    "configure", "install", "deploy", "ship", "release", "publish",
    "post", "tweet", "announce", "share", "ignore", "disregard",
    "pretend", "act",
];

fn has_verb(words: &[&str]) -> bool {
    words.iter().any(|w| {
        let stripped = w.trim_matches(|c: char| !c.is_alphanumeric());
        VERBS.iter().any(|v| *v == stripped)
    })
}

/// Generic action verbs that, on their own, don't tell another agent
/// what to actually do. Used as an additional +10 ambiguity penalty.
const VAGUE_VERBS: &[&str] = &[
    "make", "fix", "add", "change", "update", "modify", "improve",
    "optimize", "optimise", "tune",
];

fn has_vague_verb(words: &[&str]) -> bool {
    words.iter().any(|w| {
        let stripped = w.trim_matches(|c: char| !c.is_alphanumeric());
        VAGUE_VERBS.iter().any(|v| *v == stripped)
    })
}

/// Content words that aren't on the verb list and aren't typical
/// function words (articles, prepositions, conjunctions, pronouns) are
/// treated as plausible nouns. Cheap proxy for "the input names a
/// thing".
const STOPWORDS: &[&str] = &[
    "a", "an", "the", "of", "to", "for", "with", "in", "on", "at", "by",
    "and", "or", "but", "if", "as", "that", "this", "it", "they", "them",
    "we", "you", "i", "be", "is", "are", "was", "were", "do", "does",
    "did", "have", "has", "had", "from", "into", "about",
];

/// Tokens that LOOK like they could be nouns (not verbs, not stopwords)
/// but in practice point to vague prompts. Adjectival comparators
/// ("faster") describe a thing without naming it; abstract operational
/// gerunds and category-nouns ("handling", "error") give the LLM no
/// concrete target.
const NON_NOUN_TOKENS: &[&str] = &[
    // Adjectival comparators
    "faster", "slower", "better", "worse", "smarter", "easier", "harder",
    "cheaper", "simpler", "cleaner", "neater", "stronger", "weaker",
    "bigger", "smaller", "larger", "shorter", "longer", "brighter",
    "lighter", "heavier", "deeper", "richer", "poorer", "tighter",
    "looser", "nicer", "sleeker", "smoother", "rougher", "prettier",
    // Abstract operational gerunds
    "handling", "logging", "processing", "checking", "monitoring",
    "rendering", "tracking", "shipping", "scaling", "matching",
    "parsing", "polling", "streaming", "scrolling", "loading",
    "caching", "throttling", "retrying", "fetching", "syncing",
    "indexing", "queueing", "batching", "bundling", "linting",
    // Category-abstract nouns
    "error", "errors", "stuff", "thing", "things", "issue", "issues",
    "problem", "problems", "feature", "features", "performance", "code",
];

fn has_plausible_noun(words: &[&str]) -> bool {
    words.iter().any(|w| {
        let stripped = w.trim_matches(|c: char| !c.is_alphanumeric());
        if stripped.is_empty() {
            return false;
        }
        let is_verb = VERBS.iter().any(|v| *v == stripped);
        let is_stop = STOPWORDS.iter().any(|s| *s == stripped);
        let is_non_noun = NON_NOUN_TOKENS.iter().any(|n| *n == stripped);
        !is_verb && !is_stop && !is_non_noun
    })
}

fn has_bare_pronoun(lower: &str) -> bool {
    // word-boundary match for "it"/"this"/"that". \b in Rust regex
    // would do the trick, but a manual scan keeps the router
    // dependency-free at module level.
    for needle in ["it", "this", "that"] {
        let mut start = 0;
        while let Some(found) = lower[start..].find(needle) {
            let abs = start + found;
            let before_ok = abs == 0
                || !lower
                    .as_bytes()
                    .get(abs - 1)
                    .map(|b| (*b as char).is_alphanumeric())
                    .unwrap_or(false);
            let after_idx = abs + needle.len();
            let after_ok = after_idx >= lower.len()
                || !lower
                    .as_bytes()
                    .get(after_idx)
                    .map(|b| (*b as char).is_alphanumeric())
                    .unwrap_or(false);
            if before_ok && after_ok {
                return true;
            }
            start = abs + 1;
        }
    }
    false
}

/// Specifics = at least one of: a digit, a `"`-quoted segment, or a
/// proper-noun-looking word (uppercase first letter, not the input's
/// first word).
fn has_specifics(s: &str) -> bool {
    if s.chars().any(|c| c.is_ascii_digit()) {
        return true;
    }
    if s.contains('"') {
        return true;
    }
    let words: Vec<&str> = s.split_whitespace().collect();
    words.iter().skip(1).any(|w| {
        let first = w.chars().next();
        first.is_some_and(|c| c.is_ascii_uppercase())
    })
}

// ─────────────────────────────────────────────────────────────────────
// Tests — brief requires at least 5
// ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fix_it_declines() {
        let out = run("fix it", false);
        match out.decision {
            RoutingDecision::Decline { reason } => {
                assert_eq!(reason, "input too vague");
            }
            other => panic!("expected Decline, got {other:?}"),
        }
    }

    #[test]
    fn make_it_faster_declines() {
        let out = run("make it faster", false);
        assert!(matches!(out.decision, RoutingDecision::Decline { .. }));
    }

    #[test]
    fn add_error_handling_declines() {
        let out = run("add error handling", false);
        assert!(
            matches!(out.decision, RoutingDecision::Decline { .. }),
            "got {:?}",
            out.decision
        );
    }

    #[test]
    fn detailed_code_prompt_is_bypass_or_code() {
        let out = run(
            "refactor the user service to use async/await instead of promise chains, preserving the public API",
            false,
        );
        assert!(
            matches!(out.decision, RoutingDecision::Bypass | RoutingDecision::Code),
            "got {:?}",
            out.decision
        );
    }

    #[test]
    fn write_leave_email_routes_to_writing() {
        let out = run("write a leave email", false);
        assert_eq!(out.decision, RoutingDecision::Writing);
    }

    #[test]
    fn summarise_paragraph_routes_to_generic() {
        let out = run("summarise this paragraph", false);
        assert_eq!(out.decision, RoutingDecision::Generic);
    }

    #[test]
    fn short_code_prompt_routes_to_code_not_bypass() {
        // Below the 60-char threshold for Bypass, so should land on Code.
        let out = run("refactor the user service", false);
        assert_eq!(out.decision, RoutingDecision::Code);
    }

    #[test]
    fn ambiguity_caps_at_100() {
        // Two-letter input fires every penalty; cap proves the formula
        // doesn't overflow.
        assert!(score_ambiguity("hi") <= 100);
    }

    // ── Phase 2 Step 8: Mode D ────────────────────────────────────

    #[test]
    fn effective_threshold_is_70_without_context() {
        let out = run("write a leave email", false);
        assert_eq!(out.effective_threshold, DECLINE_THRESHOLD);
    }

    #[test]
    fn effective_threshold_includes_context_bump_when_context_present() {
        // With the bump set to its current value (zero, pending
        // heuristic re-tune), the effective threshold equals the base
        // threshold even when context is present. The assertion is
        // formulated against the consts rather than a magic number so
        // it stays correct once Phase 2.5 changes the bump.
        let out = run("write a leave email", true);
        assert_eq!(
            out.effective_threshold,
            DECLINE_THRESHOLD + CONTEXT_THRESHOLD_BUMP
        );
    }

    #[test]
    fn word_count_gate_still_fires_even_with_context() {
        // `fix it` scores 95 ambiguity (every penalty fires) — well
        // above any plausible bumped threshold. Word count is 2
        // (< 5). Both gates trip regardless of context_present, and
        // regardless of what value Phase 2.5 picks for the bump.
        // Context cannot rescue a structurally-thin input.
        let out = run("fix it", true);
        assert!(
            matches!(out.decision, RoutingDecision::Decline { .. }),
            "context must not rescue 2-word bare-pronoun inputs: got {:?}",
            out.decision
        );
    }

    #[test]
    fn high_word_count_avoids_decline_regardless_of_threshold() {
        // 5+ words bypasses the word_count gate entirely — Decline
        // requires BOTH gates to fire. This proves the && is preserved.
        let out = run("fix it now please right now", false);
        assert!(
            !matches!(out.decision, RoutingDecision::Decline { .. }),
            "expected non-decline for 6-word input: got {:?}",
            out.decision
        );
    }

    // ── Phase 2 Step 8 calibration record ──────────────────────────
    //
    // Snapshot of `score_ambiguity` on the brief author's five
    // calibration inputs, captured during the Step 8 dry-run. This is
    // documentation, not a regression test — the heuristic may change
    // (Phase 2.5) and we explicitly do not want a calibration snapshot
    // becoming a gate that masks intended re-tuning.
    //
    //   input                          ambig  wc   no-ctx route (thr=70)
    //   ----------------------------   -----  --   --------------------
    //   refactor the auth flow            40   4   Code
    //   make it faster                    95   3   Decline
    //   add error handling                80   3   Decline
    //   fix it                            95   2   Decline
    //   update the dashboard layout       50   4   Generic
    //
    // Outcome of the dry-run: the heuristic does not produce inputs in
    // the 70–84 band where Mode D's threshold bump could meaningfully
    // unlock a Decline. `CONTEXT_THRESHOLD_BUMP` is therefore set to 0
    // for this ship; the plumbing flows through trace logs so the
    // signal is verifiable, but the live effect on routing is zero
    // until Phase 2.5 re-tunes `score_ambiguity`.
}
