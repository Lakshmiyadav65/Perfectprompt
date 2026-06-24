//! Stage 1 of the polish pipeline.
//!
//! Build a compact record of HOW the user writes, not just WHAT they
//! wrote. The fingerprint is the source of truth for "voice" — it
//! drives the constraint block we inject into the LLM prompt (Stage 2)
//! and the drift check we run on the LLM's output (Stage 4).
//!
//! Pure Rust pattern matching. No LLM call. Designed to run in ~5 ms on
//! a typical 200-char input so it's invisible on the polish hot path.
//!
//! Detection rules are codified explicitly: each signal has a small,
//! testable matcher. The intent is calibration over cleverness — every
//! rule listed in the polish spec maps 1:1 to a check in `fingerprint()`,
//! and every check is exercised by a unit test below.

use std::collections::HashSet;

/// The complete voice snapshot. See module-level comments for what each
/// field encodes. Cloneable so the diff check at Stage 4 can hold input
/// and output snapshots side-by-side cheaply.
#[derive(Debug, Clone, PartialEq)]
pub struct VoiceFingerprint {
    pub register: Register,
    pub cultural_markers: Vec<String>,
    pub has_greeting: bool,
    pub has_signoff: bool,
    pub uses_lowercase_i: bool,
    pub contractions_used: Vec<String>,
    pub emojis_present: bool,
    pub emoji_list: Vec<String>,
    pub casual_vocab_used: Vec<String>,
    pub sentence_count: usize,
    pub avg_words_per_sentence: f32,
    pub is_question: bool,
    pub punctuation_style: PunctuationStyle,
    pub capitalization_style: CapitalizationStyle,
    pub emotion: Emotion,
    pub word_count: usize,
    /// Substring presence of the canonical warmth tokens. Used by
    /// Stage 4's warmth-stripping check: the emotion classifier can
    /// say "Apologetic" for both "sorry" and "I apologize", but the
    /// user's casual warmth is only carried by the first. Tracking
    /// these as booleans lets the diff catch "sorry" → "I apologize"
    /// even when emotion fingerprint stays the same.
    ///
    /// `has_sorry_token` is `lower.contains("sorry")`.
    /// `has_thanks_token` is `lower.contains("thank")` — catches
    /// both "thanks" and "thank you" but NOT "I appreciate".
    pub has_sorry_token: bool,
    pub has_thanks_token: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Register {
    Casual,
    Neutral,
    Formal,
    /// Input mixes casual and formal triggers (e.g., "Dear sir, lol")
    /// — register-flip is allowed because the user themselves was
    /// already mixed.
    Mixed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PunctuationStyle {
    Standard,
    /// `!!`, `???`, `...` etc. — emphatic punctuation.
    Heavy,
    /// No terminal punctuation in the input at all.
    Minimal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapitalizationStyle {
    /// First letter capitalized, rest sentence-cased.
    Sentence,
    /// Entire input lowercase (no uppercase letters present).
    Lowercase,
    /// Entire input uppercase (no lowercase letters present).
    AllCaps,
    Mixed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Emotion {
    Apologetic,
    Grateful,
    Urgent,
    Excited,
    Frustrated,
    Neutral,
}

// ─────────────────────────────────────────────────────────────────────
// Trigger tables. Each table is the authoritative list for one signal.
// Adding a casual marker only requires touching one of these.
// ─────────────────────────────────────────────────────────────────────

/// Words that mark CASUAL register. Match is whole-word and
/// case-insensitive — see `contains_word`.
const CASUAL_TRIGGERS: &[&str] = &[
    "bro", "yo", "hey", "lol", "lmao", "haha", "tbh", "imo",
    "wanna", "gonna", "kinda",
];

/// Words that mark FORMAL register. Match is whole-word and
/// case-insensitive. Multi-word entries (e.g. "yours truly") are
/// checked via `contains_phrase`.
const FORMAL_TRIGGERS: &[&str] = &[
    "sir", "ma'am", "dear", "kindly", "regards", "sincerely",
    "respectfully", "yours truly",
];

/// Cultural address forms / vocatives. Casing in the output is the
/// user's casing — we preserve "Sir" as "Sir" and "sir" as "sir".
const CULTURAL_MARKERS: &[&str] = &[
    "sir", "ma'am", "bro", "mate", "y'all", "bhai", "boss", "buddy",
];

/// Tokens that, if present in the first two words of the input,
/// constitute a greeting. "good morning" / "good evening" are checked
/// as a phrase because they span two tokens.
const GREETING_SINGLE: &[&str] = &["hi", "hello", "hey", "dear", "greetings"];
const GREETING_PHRASES: &[&str] = &["good morning", "good evening", "good afternoon"];

/// Tokens that, if present in the last three words of the input,
/// constitute a sign-off.
const SIGNOFF_TOKENS: &[&str] = &[
    "regards", "thanks", "thx", "yours", "sincerely", "cheers",
    "best", "tc", "ty",
];

/// Contractions worth tracking. We don't need every contraction in
/// English — only the ones that signal a casual/neutral register that
/// the polished output should preserve.
const CONTRACTIONS: &[&str] = &[
    "i'm", "don't", "can't", "won't", "isn't", "wasn't", "didn't",
    "doesn't", "you're", "they're", "it's", "we're", "i've", "i'd",
    "i'll", "we'll",
];

/// Casual vocabulary. These survive polishing — "lol" stays as "lol",
/// not "haha" and not "(laughing)".
const CASUAL_VOCAB: &[&str] = &[
    "lol", "lmao", "haha", "tbh", "imo", "imho", "wanna", "gonna",
    "kinda", "sorta", "pls", "plz", "thx", "u", "ur", "smh", "fr",
    "ngl",
];

/// Question hints — used in addition to a trailing `?` to flag
/// rhetorical-but-actually-asking inputs like "can you send the file".
const QUESTION_HINTS: &[&str] = &[
    "can", "could", "would", "may i", "could you", "would you",
];

// ─────────────────────────────────────────────────────────────────────
// Main entry point
// ─────────────────────────────────────────────────────────────────────

/// Build the fingerprint for an input. Lowercase normalization is done
/// internally for matching; original casing is preserved in any signal
/// that surfaces back to the LLM (cultural_markers, emoji_list,
/// contractions_used, casual_vocab_used).
pub fn fingerprint(input: &str) -> VoiceFingerprint {
    let lower = input.to_lowercase();

    // ── Register ──────────────────────────────────────────────────
    let mut has_casual = false;
    let mut has_formal = false;
    for trig in CASUAL_TRIGGERS {
        if contains_word(&lower, trig) {
            has_casual = true;
            break;
        }
    }
    for trig in FORMAL_TRIGGERS {
        if trig.contains(' ') {
            if lower.contains(trig) {
                has_formal = true;
                break;
            }
        } else if contains_word(&lower, trig) {
            has_formal = true;
            break;
        }
    }
    let register = match (has_casual, has_formal) {
        (true, true) => Register::Mixed,
        (true, false) => Register::Casual,
        (false, true) => Register::Formal,
        (false, false) => Register::Neutral,
    };

    // ── Cultural markers — preserve original casing ───────────────
    let cultural_markers = collect_original_casing(input, CULTURAL_MARKERS);

    // ── Greeting (first 2 words) ─────────────────────────────────
    let first_words = first_n_words(&lower, 2);
    let mut has_greeting = false;
    for g in GREETING_SINGLE {
        if first_words.iter().any(|w| trim_punct(w) == *g) {
            has_greeting = true;
            break;
        }
    }
    if !has_greeting {
        // Phrase form: check the lowercased prefix for "good morning"
        // etc. — using a small prefix avoids false positives like
        // "have a good morning everyone".
        let prefix: String = lower.chars().take(40).collect();
        for phrase in GREETING_PHRASES {
            if prefix.starts_with(phrase) {
                has_greeting = true;
                break;
            }
        }
    }

    // ── Sign-off (last 3 words) ──────────────────────────────────
    let last_words = last_n_words(&lower, 3);
    let mut has_signoff = false;
    for s in SIGNOFF_TOKENS {
        if last_words.iter().any(|w| trim_punct(w) == *s) {
            has_signoff = true;
            break;
        }
    }

    // ── Standalone lowercase "i" ─────────────────────────────────
    // The space-padded check from the spec; for inputs that start
    // with "i " we still want this true, so we also check word-by-
    // word against the original-cased input.
    let uses_lowercase_i = input
        .split_whitespace()
        .any(|w| trim_punct_owned(w) == "i");

    // ── Contractions ─────────────────────────────────────────────
    let contractions_used = collect_original_casing(input, CONTRACTIONS);

    // ── Emojis ───────────────────────────────────────────────────
    let mut emoji_list: Vec<String> = Vec::new();
    for ch in input.chars() {
        if is_emoji(ch) {
            let s = ch.to_string();
            if !emoji_list.contains(&s) {
                emoji_list.push(s);
            }
        }
    }
    let emojis_present = !emoji_list.is_empty();

    // ── Casual vocabulary ────────────────────────────────────────
    let casual_vocab_used = collect_original_casing(input, CASUAL_VOCAB);

    // ── Sentence count + average words per sentence ──────────────
    let sentences: Vec<&str> = split_sentences(input);
    let sentence_count = sentences.len().max(1);
    let total_words: usize = sentences
        .iter()
        .map(|s| s.split_whitespace().count())
        .sum();
    let avg_words_per_sentence = if sentence_count == 0 {
        0.0
    } else {
        total_words as f32 / sentence_count as f32
    };
    let word_count = input.split_whitespace().count();

    // ── Question detection ───────────────────────────────────────
    let trimmed = input.trim_end();
    let mut is_question = trimmed.ends_with('?');
    if !is_question {
        for hint in QUESTION_HINTS {
            if hint.contains(' ') {
                if lower.contains(hint) {
                    is_question = true;
                    break;
                }
            } else if contains_word(&lower, hint) {
                is_question = true;
                break;
            }
        }
    }

    // ── Punctuation style ────────────────────────────────────────
    let punctuation_style = if input.contains("!!")
        || input.contains("???")
        || input.contains("...")
        || input.contains("…")
    {
        PunctuationStyle::Heavy
    } else if !trimmed.ends_with('.')
        && !trimmed.ends_with('!')
        && !trimmed.ends_with('?')
    {
        PunctuationStyle::Minimal
    } else {
        PunctuationStyle::Standard
    };

    // ── Capitalization style ─────────────────────────────────────
    let has_upper = input.chars().any(|c| c.is_ascii_uppercase());
    let has_lower = input.chars().any(|c| c.is_ascii_lowercase());
    let first_alpha = input.chars().find(|c| c.is_ascii_alphabetic());
    let capitalization_style = match (has_upper, has_lower) {
        (false, true) => CapitalizationStyle::Lowercase,
        (true, false) => CapitalizationStyle::AllCaps,
        (true, true) => {
            if matches!(first_alpha, Some(c) if c.is_ascii_uppercase()) {
                CapitalizationStyle::Sentence
            } else {
                CapitalizationStyle::Mixed
            }
        }
        (false, false) => CapitalizationStyle::Mixed,
    };

    // ── Emotion (ordered: most semantic-specific first) ──────────
    // Urgent goes first because its triggers ("asap", "urgent",
    // "immediately") are unambiguous — they're never used as polite
    // softeners the way "please" / "pls" are. Apologetic comes next
    // so "sorry, thanks for letting me know" still reads as Apologetic
    // rather than Grateful. Excited / Frustrated are last because
    // their triggers tend to overlap with other emotions.
    let emotion = if has_any_substr(&lower, &["asap", "urgent", "immediately", "right now"])
        || contains_word(&lower, "fast")
    {
        Emotion::Urgent
    } else if has_any_substr(&lower, &["sorry", "apolog", "excuse", "forgive", "my bad"])
        || contains_word(&lower, "pls")
        || contains_word(&lower, "please")
    {
        Emotion::Apologetic
    } else if has_any_substr(&lower, &["thank", "appreciate", "grateful"])
        || contains_word(&lower, "thx")
        || contains_word(&lower, "ty")
    {
        Emotion::Grateful
    } else if input.contains("!!")
        || has_any_substr(&lower, &["awesome", "amazing", "yay", "great"])
    {
        Emotion::Excited
    } else if has_any_substr(&lower, &["ugh", "wtf", "annoying", "frustrated"]) {
        Emotion::Frustrated
    } else {
        Emotion::Neutral
    };

    // Warmth tokens — substring checks on the lowercased text.
    // Tracked separately from emotion so the Stage 4 diff can catch
    // "sorry" → "I apologize" (emotion stays Apologetic on both sides,
    // but the user's warmth word was replaced).
    let has_sorry_token = lower.contains("sorry");
    let has_thanks_token = lower.contains("thank");

    VoiceFingerprint {
        register,
        cultural_markers,
        has_greeting,
        has_signoff,
        uses_lowercase_i,
        contractions_used,
        emojis_present,
        emoji_list,
        casual_vocab_used,
        sentence_count,
        avg_words_per_sentence,
        is_question,
        punctuation_style,
        capitalization_style,
        emotion,
        word_count,
        has_sorry_token,
        has_thanks_token,
    }
}

/// Build the VOICE SIGNATURE block that gets appended to the LLM user
/// message just before the `<input>` tags. This block is what makes
/// voice preservation a contract with the model — Stage 4's diff check
/// is what makes it enforceable.
pub fn render_voice_signature(fp: &VoiceFingerprint) -> String {
    let register = match fp.register {
        Register::Casual => "Casual",
        Register::Neutral => "Neutral",
        Register::Formal => "Formal",
        Register::Mixed => "Mixed",
    };
    let cultural = if fp.cultural_markers.is_empty() {
        "(none)".to_string()
    } else {
        fp.cultural_markers.join(", ")
    };
    let greeting_line = if fp.has_greeting {
        "yes".to_string()
    } else {
        "no — do not add one".to_string()
    };
    let signoff_line = if fp.has_signoff {
        "yes".to_string()
    } else {
        "no — do not add one".to_string()
    };
    let emojis_line = if fp.emojis_present {
        format!("user used {} — keep them", fp.emoji_list.join(" "))
    } else {
        "none — do not add any".to_string()
    };
    // Split casual vocabulary into tonal markers (must preserve) and
    // texting shortcuts (free to expand for grammar). Same split as
    // voice_diff::TONAL_PRESERVE — keep these two lists in sync.
    const TONAL: &[&str] = &["lol", "lmao", "haha", "smh"];
    let (tonal_terms, shortcut_terms): (Vec<&String>, Vec<&String>) = fp
        .casual_vocab_used
        .iter()
        .partition(|t| TONAL.iter().any(|m| m.eq_ignore_ascii_case(t)));
    let casual_vocab = if fp.casual_vocab_used.is_empty() {
        "(none)".to_string()
    } else if tonal_terms.is_empty() {
        format!(
            "shortcuts: {} — OK to expand (e.g. \"pls\"→\"please\", \"u\"→\"you\")",
            shortcut_terms
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )
    } else if shortcut_terms.is_empty() {
        format!(
            "tonal markers: {} — MUST keep verbatim",
            tonal_terms
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )
    } else {
        format!(
            "tonal markers: {} (MUST keep); shortcuts: {} (OK to expand)",
            tonal_terms
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            shortcut_terms
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    let lowercase_i = if fp.uses_lowercase_i {
        "user writes 'i' lowercase — fixing to 'I' is OK, this is the only allowed identity change".to_string()
    } else {
        "n/a".to_string()
    };
    let emotion = match fp.emotion {
        Emotion::Apologetic => "Apologetic",
        Emotion::Grateful => "Grateful",
        Emotion::Urgent => "Urgent",
        Emotion::Excited => "Excited",
        Emotion::Frustrated => "Frustrated",
        Emotion::Neutral => "Neutral",
    };

    format!(
        "VOICE SIGNATURE — preserve all of these in your output:\n\
        - Register: {register}\n\
        - Cultural markers used: {cultural}\n\
        - Has greeting: {greeting_line}\n\
        - Has sign-off: {signoff_line}\n\
        - Emojis: {emojis_line}\n\
        - Casual vocabulary: {casual_vocab}\n\
        - Lowercase 'i' style: {lowercase_i}\n\
        - Sentence count: ~{sc}\n\
        - Emotion: {emotion}\n\
        \n\
        Fix grammar, spelling, missing punctuation, broken sentence \
        structure, word-order errors, AND expand texting shortcuts \
        (pls→please, u→you, ur→your, wanna→want to, gonna→going to, \
        thx→thanks) — those are part of normal grammar polishing. Do \
        NOT change register, do NOT remove cultural markers (sir, bro, \
        mate, etc.), do NOT add greetings/signoffs the user didn't \
        write, do NOT remove emojis, do NOT remove tonal markers \
        (lol, haha, lmao, smh). Output the polished message and \
        NOTHING else — no preamble, no quotes, no explanation, no \
        markdown.",
        sc = fp.sentence_count,
    )
}

// ─────────────────────────────────────────────────────────────────────
// Helpers — small and unit-testable
// ─────────────────────────────────────────────────────────────────────

/// Whole-word containment check on a pre-lowercased haystack. "lol" in
/// "lolcat" should NOT match — we only want standalone words.
fn contains_word(lower_haystack: &str, needle: &str) -> bool {
    for word in lower_haystack.split(|c: char| !c.is_alphanumeric() && c != '\'') {
        if word == needle {
            return true;
        }
    }
    false
}

/// For each needle in `needles`, find the first whole-word occurrence
/// in `input` (case-insensitive) and emit it using the input's casing.
/// Order in the output follows the order needles appear in the input,
/// not the order of the `needles` slice — so the LLM sees them in the
/// same order the user wrote them.
fn collect_original_casing(input: &str, needles: &[&str]) -> Vec<String> {
    let needle_set: HashSet<String> = needles.iter().map(|s| s.to_lowercase()).collect();
    let mut seen: HashSet<String> = HashSet::new();
    let mut out: Vec<String> = Vec::new();
    for raw in input.split_whitespace() {
        let token = trim_punct_owned(raw);
        let key = token.to_lowercase();
        if needle_set.contains(&key) && !seen.contains(&key) {
            seen.insert(key);
            out.push(token);
        }
    }
    out
}

/// Lowercased first N whitespace-separated tokens. Punctuation is left
/// attached so callers can use `trim_punct` for the word-level comparison.
fn first_n_words(lower: &str, n: usize) -> Vec<String> {
    lower
        .split_whitespace()
        .take(n)
        .map(|s| s.to_string())
        .collect()
}

/// Lowercased last N whitespace-separated tokens.
fn last_n_words(lower: &str, n: usize) -> Vec<String> {
    let toks: Vec<&str> = lower.split_whitespace().collect();
    let start = toks.len().saturating_sub(n);
    toks[start..].iter().map(|s| s.to_string()).collect()
}

/// Strip trailing/leading non-alphanumeric punctuation but preserve
/// internal characters like the apostrophe in `"i'm"`.
fn trim_punct(word: &str) -> &str {
    let bytes = word.as_bytes();
    let mut start = 0;
    let mut end = bytes.len();
    while start < end
        && !bytes[start].is_ascii_alphanumeric()
        && bytes[start] != b'\''
    {
        start += 1;
    }
    while end > start
        && !bytes[end - 1].is_ascii_alphanumeric()
        && bytes[end - 1] != b'\''
    {
        end -= 1;
    }
    &word[start..end]
}

fn trim_punct_owned(word: &str) -> String {
    trim_punct(word).to_string()
}

/// Split a body of text into sentences using terminal `.!?` followed
/// by whitespace as the boundary. Falls back to "the whole input is
/// one sentence" if there are no terminators.
fn split_sentences(input: &str) -> Vec<&str> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    let mut sentences: Vec<&str> = Vec::new();
    let bytes = trimmed.as_bytes();
    let mut start = 0;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'.' || b == b'!' || b == b'?' {
            // Consume runs of terminator punctuation (e.g. "!!", "?!").
            let mut j = i + 1;
            while j < bytes.len() && (bytes[j] == b'.' || bytes[j] == b'!' || bytes[j] == b'?') {
                j += 1;
            }
            // A sentence break requires trailing whitespace (or EOF).
            if j >= bytes.len() || (bytes[j] as char).is_whitespace() {
                let seg = trimmed[start..j].trim();
                if !seg.is_empty() {
                    sentences.push(seg);
                }
                // Skip the whitespace gap.
                while j < bytes.len() && (bytes[j] as char).is_whitespace() {
                    j += 1;
                }
                start = j;
                i = j;
                continue;
            }
        }
        i += 1;
    }
    if start < bytes.len() {
        let seg = trimmed[start..].trim();
        if !seg.is_empty() {
            sentences.push(seg);
        }
    }
    sentences
}

/// Plain Unicode codepoint range check for the three emoji blocks
/// the spec calls out. Avoids pulling unicode-segmentation just for
/// this — the ranges cover all standard pictographs we expect.
fn is_emoji(c: char) -> bool {
    let cp = c as u32;
    (0x1F300..=0x1F9FF).contains(&cp) // Misc symbols & pictographs / supplemental
        || (0x2600..=0x27BF).contains(&cp) // Misc symbols + dingbats
        || (0x1FA70..=0x1FAFF).contains(&cp) // Symbols & pictographs extended-A
}

fn has_any_substr(lower: &str, needles: &[&str]) -> bool {
    needles.iter().any(|n| lower.contains(n))
}

// ─────────────────────────────────────────────────────────────────────
// Tests — one per detection rule, plus the eight inputs from the brief.
// ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Register ──────────────────────────────────────────────────

    #[test]
    fn register_casual_from_bro_and_lol() {
        let fp = fingerprint("bro i cant come today lol some urgent work");
        assert_eq!(fp.register, Register::Casual);
    }

    #[test]
    fn register_formal_from_sir() {
        let fp = fingerprint("sir my internet not working so i joined late");
        assert_eq!(fp.register, Register::Formal);
    }

    #[test]
    fn register_neutral_when_no_triggers() {
        let fp = fingerprint("I have made the changes. Please check them.");
        // "please" is not in the formal trigger list (only "kindly",
        // "regards", etc. are). This is intentional — "please" is
        // neutral-polite, not formal-register.
        assert_eq!(fp.register, Register::Neutral);
    }

    #[test]
    fn register_mixed_when_both() {
        let fp = fingerprint("Dear sir, lol that was funny");
        assert_eq!(fp.register, Register::Mixed);
    }

    // ── Cultural markers ──────────────────────────────────────────

    #[test]
    fn cultural_markers_preserve_user_casing() {
        let fp = fingerprint("Sir, please check");
        assert_eq!(fp.cultural_markers, vec!["Sir".to_string()]);
        let fp2 = fingerprint("sir please check");
        assert_eq!(fp2.cultural_markers, vec!["sir".to_string()]);
    }

    #[test]
    fn cultural_markers_collects_multiple() {
        let fp = fingerprint("bro tell boss what mate said");
        // Order should follow input order: bro, boss, mate.
        assert_eq!(
            fp.cultural_markers,
            vec!["bro".to_string(), "boss".to_string(), "mate".to_string()]
        );
    }

    #[test]
    fn cultural_markers_empty_when_none_present() {
        let fp = fingerprint("just a normal sentence");
        assert!(fp.cultural_markers.is_empty());
    }

    // ── Greeting / sign-off ───────────────────────────────────────

    #[test]
    fn greeting_detected_in_first_two_words() {
        assert!(fingerprint("hello team, quick update").has_greeting);
        assert!(fingerprint("Hi Sarah, thanks for the note").has_greeting);
        assert!(fingerprint("good morning everyone").has_greeting);
    }

    #[test]
    fn greeting_false_when_outside_first_two_words() {
        // "hi" appears at position 5 — not a greeting.
        assert!(!fingerprint("can you say hi for me").has_greeting);
    }

    #[test]
    fn signoff_detected_in_last_three_words() {
        assert!(fingerprint("Will revert soon. Regards, K").has_signoff);
        assert!(fingerprint("see you tomorrow thanks").has_signoff);
    }

    #[test]
    fn signoff_false_when_no_closing_token() {
        assert!(!fingerprint("can we meet tomorrow").has_signoff);
    }

    // ── Lowercase i ───────────────────────────────────────────────

    #[test]
    fn lowercase_i_detected_when_standalone() {
        assert!(fingerprint("can i come over").uses_lowercase_i);
        assert!(fingerprint("i done the changes").uses_lowercase_i);
    }

    #[test]
    fn lowercase_i_false_when_already_capitalized() {
        assert!(!fingerprint("I have done the changes").uses_lowercase_i);
    }

    // ── Contractions ──────────────────────────────────────────────

    #[test]
    fn contractions_collected() {
        let fp = fingerprint("I'm sorry I can't make it, won't be long");
        let lc: Vec<String> = fp.contractions_used.iter().map(|s| s.to_lowercase()).collect();
        assert!(lc.contains(&"i'm".to_string()));
        assert!(lc.contains(&"can't".to_string()));
        assert!(lc.contains(&"won't".to_string()));
    }

    // ── Emojis ────────────────────────────────────────────────────

    #[test]
    fn emoji_detection_pictographs() {
        let fp = fingerprint("ready to ship 🚀 lets go");
        assert!(fp.emojis_present);
        assert_eq!(fp.emoji_list, vec!["🚀".to_string()]);
    }

    #[test]
    fn emoji_detection_misc_symbols() {
        // Heart (U+2764) sits in U+2600..=U+27BF.
        let fp = fingerprint("ok ❤ thanks");
        assert!(fp.emojis_present);
    }

    #[test]
    fn emoji_absent_for_plain_ascii() {
        let fp = fingerprint("no emoji here just text");
        assert!(!fp.emojis_present);
        assert!(fp.emoji_list.is_empty());
    }

    // ── Casual vocabulary ─────────────────────────────────────────

    #[test]
    fn casual_vocab_collects_lol_tbh() {
        let fp = fingerprint("tbh that was funny lol");
        let lc: Vec<String> = fp.casual_vocab_used.iter().map(|s| s.to_lowercase()).collect();
        assert!(lc.contains(&"tbh".to_string()));
        assert!(lc.contains(&"lol".to_string()));
    }

    // ── Sentence counting ─────────────────────────────────────────

    #[test]
    fn sentence_count_splits_on_terminators() {
        let fp = fingerprint("First sentence. Second sentence! Third?");
        assert_eq!(fp.sentence_count, 3);
    }

    #[test]
    fn sentence_count_at_least_one_for_unterminated() {
        let fp = fingerprint("just a fragment with no terminator");
        assert_eq!(fp.sentence_count, 1);
    }

    // ── Capitalization style ──────────────────────────────────────

    #[test]
    fn capitalization_lowercase_when_no_uppercase() {
        let fp = fingerprint("bro send file fast");
        assert_eq!(fp.capitalization_style, CapitalizationStyle::Lowercase);
    }

    #[test]
    fn capitalization_sentence_when_first_letter_caps() {
        let fp = fingerprint("Bro send file fast");
        assert_eq!(fp.capitalization_style, CapitalizationStyle::Sentence);
    }

    #[test]
    fn capitalization_allcaps_when_no_lowercase() {
        let fp = fingerprint("SEND THE FILE");
        assert_eq!(fp.capitalization_style, CapitalizationStyle::AllCaps);
    }

    // ── Punctuation style ─────────────────────────────────────────

    #[test]
    fn punctuation_heavy_for_double_bang() {
        let fp = fingerprint("amazing!! love it");
        assert_eq!(fp.punctuation_style, PunctuationStyle::Heavy);
    }

    #[test]
    fn punctuation_minimal_when_no_terminator() {
        let fp = fingerprint("just a thought");
        assert_eq!(fp.punctuation_style, PunctuationStyle::Minimal);
    }

    #[test]
    fn punctuation_standard_with_single_period() {
        let fp = fingerprint("Got it.");
        assert_eq!(fp.punctuation_style, PunctuationStyle::Standard);
    }

    // ── Emotion ───────────────────────────────────────────────────

    #[test]
    fn emotion_apologetic() {
        let fp = fingerprint("sorry for the delay, my bad");
        assert_eq!(fp.emotion, Emotion::Apologetic);
    }

    #[test]
    fn emotion_grateful() {
        let fp = fingerprint("thank you so much for the help");
        assert_eq!(fp.emotion, Emotion::Grateful);
    }

    #[test]
    fn emotion_urgent() {
        let fp = fingerprint("need this asap please");
        assert_eq!(fp.emotion, Emotion::Urgent);
    }

    #[test]
    fn emotion_neutral_default() {
        let fp = fingerprint("the file is here");
        assert_eq!(fp.emotion, Emotion::Neutral);
    }

    // ── is_question ───────────────────────────────────────────────

    #[test]
    fn is_question_from_trailing_qmark() {
        assert!(fingerprint("Are you coming?").is_question);
    }

    #[test]
    fn is_question_from_hint_word_without_qmark() {
        assert!(fingerprint("can you send the file").is_question);
    }

    // ── Spec acceptance test #1: casual + bro + lol ───────────────

    #[test]
    fn acceptance_casual_input_with_bro_and_lol() {
        let fp = fingerprint("bro i cant come today lol some urgent work");
        assert_eq!(fp.register, Register::Casual);
        assert!(fp.cultural_markers.iter().any(|m| m.eq_ignore_ascii_case("bro")));
        assert!(fp.casual_vocab_used.iter().any(|m| m.eq_ignore_ascii_case("lol")));
        assert!(!fp.has_greeting);
        assert!(!fp.has_signoff);
        assert!(fp.uses_lowercase_i);
    }

    // ── Spec acceptance test #2: formal "sir" with lowercase i ────

    #[test]
    fn acceptance_formal_sir_lowercase_preserved() {
        let fp = fingerprint("sir my internet not working so i joined late");
        assert_eq!(fp.register, Register::Formal);
        // "sir" is a cultural marker AND a formal trigger — we
        // collect the casing the user wrote.
        assert!(fp.cultural_markers.iter().any(|m| m == "sir"));
        assert!(!fp.has_greeting); // "sir" alone is not a greeting word
    }

    // ── render_voice_signature ────────────────────────────────────

    #[test]
    fn signature_includes_register_and_markers() {
        let fp = fingerprint("bro i cant come today lol");
        let sig = render_voice_signature(&fp);
        assert!(sig.contains("Register: Casual"));
        assert!(sig.contains("Cultural markers used: bro"));
        // "lol" is tonal — the rendered line should flag it as MUST keep.
        assert!(sig.contains("lol"));
        assert!(sig.contains("MUST keep"));
    }

    #[test]
    fn signature_flags_shortcuts_as_expandable() {
        // "pls" is a shortcut — the signature should explicitly tell
        // the LLM that expanding it is allowed.
        let fp = fingerprint("bro send fast pls");
        let sig = render_voice_signature(&fp);
        assert!(sig.contains("pls"));
        assert!(sig.contains("OK to expand"));
    }

    #[test]
    fn signature_says_do_not_add_greeting_when_absent() {
        let fp = fingerprint("can we meet tomorrow");
        let sig = render_voice_signature(&fp);
        assert!(sig.contains("Has greeting: no — do not add one"));
        assert!(sig.contains("Has sign-off: no — do not add one"));
    }

    #[test]
    fn signature_keep_emojis_when_present() {
        let fp = fingerprint("ready 🚀");
        let sig = render_voice_signature(&fp);
        assert!(sig.contains("user used 🚀"));
    }

    // ── Word-boundary regression: "lol" must not match inside "lolcat"
    // because the spec's whole-word semantics. If this broke, every
    // input containing "lola" or "lolcat" would falsely report
    // casual_vocab_used = ["lol"].
    #[test]
    fn casual_vocab_word_boundary_respected() {
        let fp = fingerprint("Lola is my dog");
        assert!(fp.casual_vocab_used.is_empty());
    }
}
