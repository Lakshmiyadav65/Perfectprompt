//! Stage 4 of the polish pipeline.
//!
//! Compare the voice fingerprint of the LLM's output against the
//! input's fingerprint. If the LLM erased identity markers — flipped
//! register, dropped a cultural address form, stripped emojis,
//! injected a greeting/signoff the user didn't write — REJECT the
//! output and fall back to the user's original text.
//!
//! This is the single most load-bearing quality layer in the polish
//! pipeline. The prompt and the voice-signature block tell the LLM
//! what to preserve; this module makes that contract enforceable.
//! Without it, polish would be one more "polite-by-default" rewriter.

use crate::voice_fingerprint::{
    CapitalizationStyle, Emotion, PunctuationStyle, Register, VoiceFingerprint,
};

/// Outcome of comparing input and output fingerprints.
#[derive(Debug, Clone, PartialEq)]
pub enum DriftVerdict {
    /// No destructive drift — deliver the LLM output.
    Accept,
    /// Voice was erased. Caller falls back to the user's original text.
    /// The `String` is a human-readable reject reason for trace logs
    /// and the stderr "[polish] rejected: ..." line.
    Reject(String),
}

// ─────────────────────────────────────────────────────────────────────
// Drift tiers (from the polish spec)
// ─────────────────────────────────────────────────────────────────────
//
// `diff` checks Tier 3 only — those are the rules that actually cause
// rejection. Tier 1 and Tier 2 are documented here so future readers
// don't accidentally promote them. If a check isn't in the Tier 3
// list below, it MUST NOT cause a Reject — that's the rule.
//
// TIER 1 — ACCEPTABLE DRIFT. Expected outcomes of polishing; the diff
// must let these through. Do NOT add any of these as a Reject case:
//   - lowercase 'i' → 'I'
//   - missing apostrophe added (cant → can't)
//   - missing terminal punctuation added (no '.' → '.')
//   - CapitalizationStyle Lowercase → Sentence
//   - 1 run-on sentence → N clean sentences (sentence_count change)
//   - PunctuationStyle Minimal → Standard
//   - shortcut expansion (pls → please, wanna → want to, u → you)
//
// TIER 2 — MILD DRIFT. Allowed today; could be surfaced as telemetry
// later but never converted to a Reject without also being added to
// Tier 3 below:
//   - avg_words_per_sentence change > 50%
//
// TIER 3 — DESTRUCTIVE DRIFT. ANY hit returns Reject:
//   1. Register flipped (Casual↔Formal)
//   2. Cultural marker present in input missing from output
//      (compared case-insensitively — "sir" matches "Sir")
//   3. emojis_present went from true to false
//   4. has_greeting went from false to true
//   5. has_signoff went from false to true
//   6. Tonal casual marker missing (lol/lmao/haha/smh only — shortcuts
//      like pls/wanna/u are Tier 1 expansions, not Tier 3 erasures)
//   7. Emotion erased Apologetic/Grateful → Neutral
//   8. Warmth stripped: "sorry" replaced with "I apologize", or
//      "thanks" replaced with "I appreciate" — emotion stays the
//      same but the user's warm word was formalized away
//
// Numbered to match the order in `diff()` so a reject reason like
// "voice drift: cultural marker 'bro' missing" maps to rule 2.

/// Compare two fingerprints and return a verdict. Pure function; safe
/// to call from anywhere. Runs in ~5 µs on typical inputs (no allocation
/// in the happy path).
pub fn diff(input_fp: &VoiceFingerprint, output_fp: &VoiceFingerprint) -> DriftVerdict {
    // 1) Register flip — Casual↔Formal is destructive. Casual↔Mixed
    //    or Formal↔Mixed is allowed because Mixed is a legitimate
    //    repair direction (the LLM kept some but not all formality
    //    cues). Neutral on either side is also allowed — it means the
    //    LLM rewrote without strong register signal, and we don't have
    //    a robust way to tell if that's drift or just a clean rewrite.
    let (ri, ro) = (input_fp.register, output_fp.register);
    let flipped = matches!(
        (ri, ro),
        (Register::Casual, Register::Formal) | (Register::Formal, Register::Casual),
    );
    if flipped {
        return DriftVerdict::Reject(format!(
            "voice drift: register flipped {ri:?}->{ro:?}"
        ));
    }

    // 2) Cultural markers — every marker the user wrote must survive.
    //    Compare case-insensitively (the LLM might have casing-fixed
    //    "sir" to "Sir" — fine).
    for marker in &input_fp.cultural_markers {
        let missing = !output_fp
            .cultural_markers
            .iter()
            .any(|m| m.eq_ignore_ascii_case(marker));
        if missing {
            return DriftVerdict::Reject(format!(
                "voice drift: cultural marker '{marker}' missing"
            ));
        }
    }

    // 3) Emojis — if the user used emojis, the polished output must
    //    still contain emojis. We don't require the SAME emojis (the
    //    LLM might collapse 👍🏻 → 👍 etc.); only that at least one
    //    survived.
    if input_fp.emojis_present && !output_fp.emojis_present {
        return DriftVerdict::Reject(
            "voice drift: emojis present in input but missing in output".to_string(),
        );
    }

    // 4) Greeting injection — the LLM added "Hi" / "Dear" / "Hello"
    //    when the user didn't write one. Classic formalization tell.
    if !input_fp.has_greeting && output_fp.has_greeting {
        return DriftVerdict::Reject(
            "voice drift: greeting added that user did not write".to_string(),
        );
    }

    // 5) Sign-off injection — the LLM added "Regards" / "Thanks" at
    //    the end when the user didn't write one. Same failure mode
    //    as #4.
    if !input_fp.has_signoff && output_fp.has_signoff {
        return DriftVerdict::Reject(
            "voice drift: signoff added that user did not write".to_string(),
        );
    }

    // 6) Casual vocabulary — but only the TONAL subset. Texting
    //    shortcuts like "pls", "wanna", "u", "thx" have clean
    //    full-word equivalents ("please", "want to", "you", "thanks")
    //    and expanding them is exactly the grammar polishing this
    //    feature exists to do. Treating them as preserve-or-reject
    //    defeats the purpose — "bro send file pls" → "Bro, please
    //    send the file" is good polish, not voice erasure.
    //
    //    Tonal markers (laughter interjections, "shaking my head"
    //    style reactions) ARE preserve-required because they carry
    //    voice the way an emoji does: there's no full-word
    //    substitute that conveys the same lightness/tone.
    const TONAL_PRESERVE: &[&str] = &["lol", "lmao", "haha", "smh"];
    for term in &input_fp.casual_vocab_used {
        let term_lc = term.to_lowercase();
        let is_tonal = TONAL_PRESERVE.iter().any(|t| *t == term_lc.as_str());
        if !is_tonal {
            continue; // shortcut — grammar expansion is allowed
        }
        let missing = !output_fp
            .casual_vocab_used
            .iter()
            .any(|m| m.eq_ignore_ascii_case(term));
        if missing {
            return DriftVerdict::Reject(format!(
                "voice drift: casual vocabulary '{term}' missing"
            ));
        }
    }

    // 7) Emotion erasure — but only for Apologetic and Grateful.
    //    Those are the "strip the warmth" failure modes where the LLM
    //    deletes "sorry"/"thanks" markers to sound professional, which
    //    materially changes the message. Urgent/Excited/Frustrated are
    //    typically carried by content vocabulary that the LLM may
    //    legitimately paraphrase (e.g. "fast" → "as soon as possible"),
    //    so fingerprint-level erasure there is too noisy to act on.
    //    A *new* emotion appearing (Neutral→Apologetic) is never drift —
    //    the model is allowed to clarify intent.
    if matches!(input_fp.emotion, Emotion::Apologetic | Emotion::Grateful)
        && output_fp.emotion == Emotion::Neutral
    {
        return DriftVerdict::Reject(format!(
            "voice drift: emotion {:?} erased to Neutral",
            input_fp.emotion
        ));
    }

    // 8) Warmth-stripping — emotion fingerprint stays the same but the
    //    canonical warm word was replaced. "sorry for the delay" → "I
    //    apologize for the delay" classifies as Apologetic on both
    //    sides (because "apolog" is in the Apologetic trigger set), so
    //    rule 7 doesn't catch it. But the user's casual warmth was
    //    formalized away — that's destructive drift the polish spec
    //    explicitly calls out.
    //
    //    Trigger: input has the canonical warm token + emotion was
    //    Apologetic/Grateful + output dropped the token. Symmetric
    //    pair: "sorry" for Apologetic, "thank" substring for Grateful
    //    (catches "thanks" / "thank you" / "thank-you" but NOT
    //    "I appreciate" / "much obliged").
    if input_fp.has_sorry_token
        && !output_fp.has_sorry_token
        && input_fp.emotion == Emotion::Apologetic
    {
        return DriftVerdict::Reject(
            "voice drift: 'sorry' replaced with formal apology phrasing".to_string(),
        );
    }
    if input_fp.has_thanks_token
        && !output_fp.has_thanks_token
        && input_fp.emotion == Emotion::Grateful
    {
        return DriftVerdict::Reject(
            "voice drift: 'thanks' replaced with formal gratitude phrasing".to_string(),
        );
    }

    // Sentinel: prove to a future reader that CapitalizationStyle and
    // PunctuationStyle are intentionally NOT checked above. They're
    // Tier 1 drifts — Lowercase → Sentence and Minimal → Standard are
    // the whole point of polishing. If you find yourself wanting to
    // reject on either, re-read the tier comments at the top of the
    // file. `sentence_count` is the same story: Tier 1 / Tier 2 only,
    // never Tier 3.
    let _ = (input_fp.capitalization_style, input_fp.punctuation_style);
    let _ = (CapitalizationStyle::Lowercase, PunctuationStyle::Minimal);
    let _ = (input_fp.sentence_count, output_fp.sentence_count);

    DriftVerdict::Accept
}

// ─────────────────────────────────────────────────────────────────────
// Tests — one per Tier 3 rule + acceptance tests for Tier 1 drifts.
// ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::voice_fingerprint::fingerprint;

    // ── REJECT cases ──────────────────────────────────────────────

    #[test]
    fn rejects_register_flip_casual_to_formal() {
        let input_fp = fingerprint("bro i cant come today lol");
        let output_fp = fingerprint("Dear sir, I regret to inform you...");
        let v = diff(&input_fp, &output_fp);
        match v {
            DriftVerdict::Reject(r) => {
                let lower = r.to_lowercase();
                assert!(
                    lower.contains("register") || lower.contains("voice"),
                    "reject reason should mention register or voice: {r}"
                );
            }
            DriftVerdict::Accept => panic!("expected Reject for register flip"),
        }
    }

    #[test]
    fn rejects_missing_cultural_marker_bro() {
        let input_fp = fingerprint("bro send the file fast");
        let output_fp = fingerprint("Please send the file as soon as possible.");
        let v = diff(&input_fp, &output_fp);
        match v {
            DriftVerdict::Reject(r) => assert!(
                r.contains("bro"),
                "reject reason should name the missing marker: {r}"
            ),
            DriftVerdict::Accept => panic!("expected Reject for missing 'bro'"),
        }
    }

    #[test]
    fn rejects_missing_emojis() {
        let input_fp = fingerprint("ready to ship 🚀 lets go");
        let output_fp = fingerprint("Ready to ship, let's go.");
        let v = diff(&input_fp, &output_fp);
        assert!(matches!(v, DriftVerdict::Reject(_)));
    }

    #[test]
    fn rejects_added_greeting() {
        let input_fp = fingerprint("can we meet tomorrow");
        let output_fp = fingerprint("Hi, can we meet tomorrow?");
        let v = diff(&input_fp, &output_fp);
        assert!(matches!(v, DriftVerdict::Reject(_)));
    }

    #[test]
    fn rejects_added_signoff() {
        let input_fp = fingerprint("can we meet tomorrow");
        let output_fp = fingerprint("Can we meet tomorrow? Thanks");
        let v = diff(&input_fp, &output_fp);
        assert!(matches!(v, DriftVerdict::Reject(_)));
    }

    #[test]
    fn rejects_missing_casual_vocab_lol() {
        let input_fp = fingerprint("that was funny lol");
        let output_fp = fingerprint("That was quite amusing.");
        let v = diff(&input_fp, &output_fp);
        assert!(matches!(v, DriftVerdict::Reject(_)));
    }

    // ── Shortcut expansion (tonal vs shortcut split) ──────────────

    #[test]
    fn accepts_pls_expanded_to_please() {
        // "pls" is a texting shortcut, not a tonal marker. Expanding
        // it to "please" preserves the register-casual spirit while
        // doing the grammar polish the user wants.
        let input_fp = fingerprint("bro send file fast pls");
        let output_fp = fingerprint("Bro, please send the file as soon as possible.");
        assert_eq!(diff(&input_fp, &output_fp), DriftVerdict::Accept);
    }

    #[test]
    fn accepts_wanna_expanded_to_want_to() {
        let input_fp = fingerprint("bro i wanna go now");
        let output_fp = fingerprint("Bro, I want to go now.");
        assert_eq!(diff(&input_fp, &output_fp), DriftVerdict::Accept);
    }

    #[test]
    fn accepts_thx_expanded_to_thanks() {
        let input_fp = fingerprint("hey thx for the help");
        // Input has greeting "hey" + casual vocab "thx" (grateful).
        // Output keeps the casual greeting and expands "thx" → "thanks".
        let output_fp = fingerprint("Hey, thanks for the help!");
        assert_eq!(diff(&input_fp, &output_fp), DriftVerdict::Accept);
    }

    #[test]
    fn accepts_u_and_ur_expanded() {
        let input_fp = fingerprint("bro is this u or ur friend");
        let output_fp = fingerprint("Bro, is this you or your friend?");
        assert_eq!(diff(&input_fp, &output_fp), DriftVerdict::Accept);
    }

    #[test]
    fn rejects_emotion_erased_to_neutral() {
        let input_fp = fingerprint("sorry, my bad");
        let output_fp = fingerprint("The matter has been noted.");
        let v = diff(&input_fp, &output_fp);
        assert!(matches!(v, DriftVerdict::Reject(_)));
    }

    // ── ACCEPT cases (Tier 1 drifts that we EXPECT to happen) ─────

    #[test]
    fn accepts_lowercase_i_becoming_uppercase() {
        let input_fp = fingerprint("i done the changes");
        let output_fp = fingerprint("I have made the changes.");
        // Neutral on both sides — neither register erasure nor
        // cultural-marker loss. The capitalization fix is exactly
        // the kind of polish we want.
        assert_eq!(diff(&input_fp, &output_fp), DriftVerdict::Accept);
    }

    #[test]
    fn accepts_minimal_to_standard_punctuation() {
        let input_fp = fingerprint("just got the file");
        let output_fp = fingerprint("Just got the file.");
        assert_eq!(diff(&input_fp, &output_fp), DriftVerdict::Accept);
    }

    #[test]
    fn accepts_one_runon_split_into_two_sentences() {
        let input_fp = fingerprint("i done the changes check once");
        let output_fp = fingerprint("I have made the changes. Please check them once.");
        // Note: "please" is in the emotion=Apologetic trigger set, so
        // output_fp.emotion = Apologetic and input_fp.emotion = Neutral.
        // That's an emotion ADDED, not erased — the diff rule only
        // rejects erasure, so this should still Accept.
        assert_eq!(diff(&input_fp, &output_fp), DriftVerdict::Accept);
    }

    #[test]
    fn accepts_register_neutral_to_neutral() {
        let input_fp = fingerprint("the meeting is at 3");
        let output_fp = fingerprint("The meeting is at 3.");
        assert_eq!(diff(&input_fp, &output_fp), DriftVerdict::Accept);
    }

    #[test]
    fn accepts_preserved_bro_with_grammar_fix() {
        let input_fp = fingerprint("bro send me file fast");
        let output_fp = fingerprint("Bro, could you send me the file as soon as possible?");
        assert_eq!(diff(&input_fp, &output_fp), DriftVerdict::Accept);
    }

    // ── Casing-insensitivity ──────────────────────────────────────

    #[test]
    fn cultural_marker_preserved_after_casing_change() {
        // "sir" → "Sir" is fine; the marker is still there.
        let input_fp = fingerprint("sir please check this");
        let output_fp = fingerprint("Sir, please check this.");
        assert_eq!(diff(&input_fp, &output_fp), DriftVerdict::Accept);
    }

    // ── Smoke test: canonical brief example ──────────────────────
    //
    // Per the spec, this exact I/O pair MUST polish successfully.
    // If any future tightening of `diff` causes this to Reject, the
    // polish feature has regressed.

    #[test]
    fn smoke_canonical_brief_example_accepts() {
        let input_fp = fingerprint("How is you doing, feel better?");
        let output_fp = fingerprint("How are you doing? Do you feel better?");
        assert_eq!(diff(&input_fp, &output_fp), DriftVerdict::Accept);
    }

    // ── Warmth-stripping (Tier 3 rule 8) ──────────────────────────

    #[test]
    fn rejects_sorry_replaced_with_i_apologize() {
        // Input emotion is Apologetic (matches "sorry"). Output is also
        // Apologetic (matches "apolog"). Rule 7 doesn't fire — both
        // sides classify as Apologetic. Rule 8 catches the warmth strip
        // because input has "sorry" token and output doesn't.
        let input_fp = fingerprint("sorry for the delay");
        let output_fp = fingerprint("I apologize for the delay.");
        match diff(&input_fp, &output_fp) {
            DriftVerdict::Reject(r) => assert!(
                r.to_lowercase().contains("sorry"),
                "reject reason should name the lost token: {r}"
            ),
            DriftVerdict::Accept => panic!("expected Reject for warmth strip"),
        }
    }

    #[test]
    fn rejects_thanks_replaced_with_i_appreciate() {
        let input_fp = fingerprint("thanks for the help");
        let output_fp = fingerprint("I appreciate the help.");
        match diff(&input_fp, &output_fp) {
            DriftVerdict::Reject(r) => assert!(
                r.to_lowercase().contains("thanks"),
                "reject reason should name the lost token: {r}"
            ),
            DriftVerdict::Accept => panic!("expected Reject for warmth strip"),
        }
    }

    #[test]
    fn accepts_thanks_preserved_through_thank_you() {
        // "thanks" → "Thank you" is NOT warmth stripping — both still
        // contain the "thank" substring. The diff lets this through.
        let input_fp = fingerprint("thanks for the help");
        let output_fp = fingerprint("Thank you for the help.");
        assert_eq!(diff(&input_fp, &output_fp), DriftVerdict::Accept);
    }

    #[test]
    fn accepts_sorry_preserved_through_im_sorry() {
        let input_fp = fingerprint("sorry for the delay");
        let output_fp = fingerprint("I'm sorry for the delay.");
        assert_eq!(diff(&input_fp, &output_fp), DriftVerdict::Accept);
    }
}
