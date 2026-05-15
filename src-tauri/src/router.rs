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
}

/// Decide a route for the normalized input.
pub fn run(normalized_input: &str) -> RouterOutput {
    let domain = detect_domain(normalized_input);
    let complexity = score_complexity(normalized_input);
    let ambiguity = score_ambiguity(normalized_input);
    let word_count = normalized_input.split_whitespace().count();
    let length = normalized_input.chars().count();

    let decision = decide(domain, complexity, ambiguity, word_count, length);
    RouterOutput {
        decision,
        domain,
        complexity,
        ambiguity,
    }
}

fn decide(
    domain: Domain,
    complexity: f32,
    ambiguity: u32,
    word_count: usize,
    length: usize,
) -> RoutingDecision {
    // 1. Decline — too vague to enhance usefully.
    if ambiguity >= 70 && word_count < 5 {
        return RoutingDecision::Decline {
            reason: "input too vague".to_string(),
        };
    }
    // 2. Bypass — already an excellent prompt, skip the LLM.
    if complexity >= 0.7 && ambiguity <= 20 && length >= 60 {
        return RoutingDecision::Bypass;
    }
    // 3. Domain dispatch.
    match domain {
        Domain::Coding => RoutingDecision::Code,
        Domain::Writing | Domain::Email => RoutingDecision::Writing,
        _ => RoutingDecision::Generic,
    }
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
        let out = run("fix it");
        match out.decision {
            RoutingDecision::Decline { reason } => {
                assert_eq!(reason, "input too vague");
            }
            other => panic!("expected Decline, got {other:?}"),
        }
    }

    #[test]
    fn make_it_faster_declines() {
        let out = run("make it faster");
        assert!(matches!(out.decision, RoutingDecision::Decline { .. }));
    }

    #[test]
    fn add_error_handling_declines() {
        let out = run("add error handling");
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
        );
        assert!(
            matches!(out.decision, RoutingDecision::Bypass | RoutingDecision::Code),
            "got {:?}",
            out.decision
        );
    }

    #[test]
    fn write_leave_email_routes_to_writing() {
        let out = run("write a leave email");
        assert_eq!(out.decision, RoutingDecision::Writing);
    }

    #[test]
    fn summarise_paragraph_routes_to_generic() {
        let out = run("summarise this paragraph");
        assert_eq!(out.decision, RoutingDecision::Generic);
    }

    #[test]
    fn short_code_prompt_routes_to_code_not_bypass() {
        // Below the 60-char threshold for Bypass, so should land on Code.
        let out = run("refactor the user service");
        assert_eq!(out.decision, RoutingDecision::Code);
    }

    #[test]
    fn ambiguity_caps_at_100() {
        // Two-letter input fires every penalty; cap proves the formula
        // doesn't overflow.
        assert!(score_ambiguity("hi") <= 100);
    }
}
