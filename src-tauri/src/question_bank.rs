use std::collections::HashSet;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Domain {
    Coding,
    Email,
    Writing,
    Research,
    Analysis,
    Generic,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ImpactDimension {
    Tone,
    Audience,
    Goal,
    Constraints,
    Format,
    Length,
    Domain,
    Other,
}

impl ImpactDimension {
    pub fn as_str(&self) -> &'static str {
        match self {
            ImpactDimension::Tone => "tone",
            ImpactDimension::Audience => "audience",
            ImpactDimension::Goal => "goal",
            ImpactDimension::Constraints => "constraints",
            ImpactDimension::Format => "format",
            ImpactDimension::Length => "length",
            ImpactDimension::Domain => "domain",
            ImpactDimension::Other => "other",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum QuestionType {
    SingleSelect,
    MultiSelect,
    FreeText,
    Chips,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneratedQuestion {
    pub id: String,
    pub question: String,
    #[serde(rename = "type")]
    pub question_type: QuestionType,
    #[serde(default)]
    pub options: Vec<String>,
    #[serde(default)]
    pub placeholder: Option<String>,
    pub impact_dimension: ImpactDimension,
    #[serde(default)]
    pub required: bool,
}

/// Lightweight keyword classifier — no LLM. Buckets the captured input into
/// one of six domains by matching trigger words from PRD Section 14.
///
/// Precedence (most-specific first): Coding > Email > Analysis > Research >
/// Writing > Generic. Coding requires either an explicit code verb
/// ("fix", "refactor", "debug", "implement", "code", "function") or a
/// "write/build" verb paired with a tech keyword (e.g. "write a Python
/// script") — this prevents general "write" inputs from being mis-classified
/// as coding.
pub fn detect_domain(input: &str) -> Domain {
    let lower = input.to_lowercase();
    let tokens = tokens_of(&lower);

    let has = |w: &str| tokens.contains(w);
    let has_any = |ws: &[&str]| ws.iter().any(|w| tokens.contains(*w));

    let tech_keyword = has_any(&[
        "typescript", "ts", "javascript", "js", "python", "py", "rust", "go",
        "golang", "java", "kotlin", "swift", "ruby", "php", "html", "css",
        "sql", "react", "vue", "angular", "node", "deno", "api", "function",
        "class", "component", "server", "database", "regex", "algorithm",
        "endpoint", "schema", "query", "binary", "compiler", "linker",
    ]);

    let coding_verb = has_any(&["fix", "refactor", "debug", "implement", "code"])
        || has("function");
    let coding_with_tech = tech_keyword && has_any(&["write", "build"]);
    if coding_verb || coding_with_tech {
        return Domain::Coding;
    }

    if has_any(&["email", "mail", "reply"])
        || lower.contains("write to ")
        || lower.contains("message to ")
    {
        return Domain::Email;
    }

    if has_any(&["analyse", "analyze", "evaluate", "assess", "compare", "review"]) {
        return Domain::Analysis;
    }

    if has_any(&["research", "summarise", "summarize", "explain", "find"])
        || lower.contains("what is")
        || lower.contains("how does")
    {
        return Domain::Research;
    }

    if has_any(&["write", "draft", "article", "post", "blog", "essay", "copy"]) {
        return Domain::Writing;
    }

    Domain::Generic
}

fn tokens_of(lower: &str) -> HashSet<&str> {
    lower
        .split(|c: char| !c.is_alphanumeric())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Heuristic complexity score in [0.0, 1.0] for "enhancement ambiguity"
/// (PRD §5.1.1). Higher = more value in asking clarifying questions.
///
/// Combines four factors:
/// - **length**: short inputs leave more room for misinterpretation.
/// - **domain**: Generic gets a bump (no specific question set to lean on);
///   classified domains get a small base.
/// - **vagueness**: pronouns like "it"/"this"/"that"/"thing" with no clear
///   referent indicate missing context.
/// - **constraints**: tone/audience/format keywords already in the input
///   reduce the need to ask.
///
/// Tuned so that the canonical PRD examples — "fix bug", "tell me a joke",
/// "explain it" — clear the default 0.6 threshold while specific, well-
/// constrained prompts fall safely below it.
pub fn score_complexity(input: &str) -> f32 {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return 0.0;
    }

    let lower = trimmed.to_lowercase();
    let word_count = lower.split_whitespace().count();

    let length_factor: f32 = match word_count {
        0 => 0.0,
        1..=3 => 0.55,
        4..=6 => 0.45,
        7..=10 => 0.35,
        11..=20 => 0.2,
        21..=40 => 0.1,
        _ => 0.0,
    };

    let domain_factor: f32 = match detect_domain(trimmed) {
        Domain::Generic => 0.2,
        _ => 0.1,
    };

    let tokens = tokens_of(&lower);

    let vagueness_factor: f32 = if ["it", "this", "that", "thing", "stuff", "something"]
        .iter()
        .any(|w| tokens.contains(*w))
    {
        0.15
    } else {
        0.0
    };

    let constraint_signals = [
        "formal",
        "informal",
        "casual",
        "concise",
        "detailed",
        "bullet",
        "table",
        "tone",
        "audience",
        "professional",
        "polite",
        "executive",
        "technical",
    ];
    let constraint_count = constraint_signals
        .iter()
        .filter(|w| tokens.contains(*w))
        .count();
    let constraint_factor: f32 = match constraint_count {
        0 => 0.0,
        1 => -0.1,
        _ => -0.2,
    };

    (length_factor + domain_factor + vagueness_factor + constraint_factor).clamp(0.0, 1.0)
}

/// Returns the static question set for a given domain (PRD §14). These act
/// as the fallback when LLM question generation fails or times out — and in
/// Phase 1 they are the primary source until Phase 2 layers LLM generation
/// on top.
pub fn static_questions_for(domain: Domain) -> Vec<GeneratedQuestion> {
    match domain {
        Domain::Coding => vec![
            chips(
                "coding_lang",
                "What language or framework?",
                &["TypeScript", "Python", "JavaScript", "Rust", "Other"],
                ImpactDimension::Domain,
            ),
            chips(
                "coding_output",
                "What should the output be?",
                &[
                    "Full function",
                    "Explanation only",
                    "With tests",
                    "Inline comment",
                    "Other",
                ],
                ImpactDimension::Format,
            ),
            chips(
                "coding_constraints",
                "Any constraints?",
                &[
                    "Don't change the API",
                    "Minimal changes",
                    "Performance-critical",
                    "Other",
                ],
                ImpactDimension::Constraints,
            ),
        ],
        Domain::Email => vec![
            chips(
                "email_audience",
                "Who is this for?",
                &["Manager", "Client", "Team", "Recruiter", "Other"],
                ImpactDimension::Audience,
            ),
            chips(
                "email_tone",
                "What tone?",
                &["Formal", "Professional", "Neutral", "Casual"],
                ImpactDimension::Tone,
            ),
            chips(
                "email_action",
                "What do you want them to do?",
                &[
                    "Approve something",
                    "Provide info",
                    "Schedule a meeting",
                    "No action needed",
                    "Other",
                ],
                ImpactDimension::Goal,
            ),
        ],
        Domain::Writing => vec![
            chips(
                "writing_audience",
                "Who is the audience?",
                &[
                    "General public",
                    "Technical readers",
                    "Executive",
                    "Students",
                    "Other",
                ],
                ImpactDimension::Audience,
            ),
            chips(
                "writing_tone",
                "What tone?",
                &[
                    "Formal",
                    "Conversational",
                    "Persuasive",
                    "Informative",
                    "Other",
                ],
                ImpactDimension::Tone,
            ),
            chips(
                "writing_length",
                "Approximate length?",
                &["Short (<200 words)", "Medium (200-500)", "Long (500+)"],
                ImpactDimension::Length,
            ),
        ],
        Domain::Research => vec![
            chips(
                "research_goal",
                "What's the goal?",
                &[
                    "Understand a concept",
                    "Compare options",
                    "Make a decision",
                    "Prepare to present",
                    "Other",
                ],
                ImpactDimension::Goal,
            ),
            chips(
                "research_structure",
                "How should it be structured?",
                &["Bullet points", "Narrative", "Table", "Step by step", "Other"],
                ImpactDimension::Format,
            ),
        ],
        Domain::Analysis => vec![
            free_text(
                "analysis_context",
                "What's the context?",
                None,
                ImpactDimension::Other,
            ),
            chips(
                "analysis_decision",
                "What decision will this inform?",
                &[
                    "Choosing a tool",
                    "Planning a project",
                    "Presenting to stakeholders",
                    "Other",
                ],
                ImpactDimension::Goal,
            ),
        ],
        Domain::Generic => vec![
            free_text(
                "generic_goal",
                "What's the main goal?",
                Some("e.g. explain to a junior dev, convince a client..."),
                ImpactDimension::Goal,
            ),
            chips(
                "generic_audience",
                "Who will use this output?",
                &["Myself", "A colleague", "A client", "An AI tool", "Other"],
                ImpactDimension::Audience,
            ),
        ],
    }
}

fn chips(
    id: &str,
    question: &str,
    options: &[&str],
    dim: ImpactDimension,
) -> GeneratedQuestion {
    GeneratedQuestion {
        id: id.to_string(),
        question: question.to_string(),
        question_type: QuestionType::Chips,
        options: options.iter().map(|s| s.to_string()).collect(),
        placeholder: None,
        impact_dimension: dim,
        required: false,
    }
}

fn free_text(
    id: &str,
    question: &str,
    placeholder: Option<&str>,
    dim: ImpactDimension,
) -> GeneratedQuestion {
    GeneratedQuestion {
        id: id.to_string(),
        question: question.to_string(),
        question_type: QuestionType::FreeText,
        options: Vec::new(),
        placeholder: placeholder.map(|s| s.to_string()),
        impact_dimension: dim,
        required: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coding_verbs_route_to_coding() {
        assert_eq!(detect_domain("fix this function"), Domain::Coding);
        assert_eq!(detect_domain("refactor the auth module"), Domain::Coding);
        assert_eq!(detect_domain("debug the segfault"), Domain::Coding);
        assert_eq!(detect_domain("implement a sort"), Domain::Coding);
        assert_eq!(detect_domain("write the function signature"), Domain::Coding);
    }

    #[test]
    fn write_plus_tech_keyword_is_coding() {
        assert_eq!(detect_domain("write a python script"), Domain::Coding);
        assert_eq!(detect_domain("build a react component"), Domain::Coding);
    }

    #[test]
    fn write_without_tech_is_writing() {
        assert_eq!(detect_domain("write a blog post"), Domain::Writing);
        assert_eq!(detect_domain("draft an article"), Domain::Writing);
    }

    #[test]
    fn email_triggers() {
        assert_eq!(detect_domain("send an email to my manager"), Domain::Email);
        assert_eq!(detect_domain("reply to john"), Domain::Email);
        assert_eq!(
            detect_domain("message to the team about deployment"),
            Domain::Email
        );
    }

    #[test]
    fn analysis_triggers() {
        assert_eq!(detect_domain("analyse the Q3 sales"), Domain::Analysis);
        assert_eq!(detect_domain("review this design doc"), Domain::Analysis);
        assert_eq!(detect_domain("compare React and Vue"), Domain::Analysis);
    }

    #[test]
    fn research_triggers() {
        assert_eq!(detect_domain("research climate impact"), Domain::Research);
        assert_eq!(detect_domain("summarise this paper"), Domain::Research);
        assert_eq!(detect_domain("what is observability"), Domain::Research);
        assert_eq!(detect_domain("explain how DNS works"), Domain::Research);
    }

    #[test]
    fn unknown_falls_back_to_generic() {
        assert_eq!(detect_domain("tell me a joke"), Domain::Generic);
        assert_eq!(detect_domain(""), Domain::Generic);
    }

    #[test]
    fn coding_beats_writing_when_both_match() {
        // "write a typescript function" — write+tech AND function => Coding
        assert_eq!(
            detect_domain("write a typescript function"),
            Domain::Coding
        );
    }

    #[test]
    fn analysis_beats_research_on_compare() {
        // PRD: "compare" is an Analysis trigger
        assert_eq!(detect_domain("compare frameworks"), Domain::Analysis);
    }

    #[test]
    fn static_bank_has_expected_question_counts() {
        assert_eq!(static_questions_for(Domain::Coding).len(), 3);
        assert_eq!(static_questions_for(Domain::Email).len(), 3);
        assert_eq!(static_questions_for(Domain::Writing).len(), 3);
        assert_eq!(static_questions_for(Domain::Research).len(), 2);
        assert_eq!(static_questions_for(Domain::Analysis).len(), 2);
        assert_eq!(static_questions_for(Domain::Generic).len(), 2);
    }

    #[test]
    fn impact_dimension_as_str_matches_serde_naming() {
        assert_eq!(ImpactDimension::Tone.as_str(), "tone");
        assert_eq!(ImpactDimension::Audience.as_str(), "audience");
        assert_eq!(ImpactDimension::Constraints.as_str(), "constraints");
    }

    #[test]
    fn empty_input_scores_zero() {
        assert_eq!(score_complexity(""), 0.0);
        assert_eq!(score_complexity("   "), 0.0);
    }

    #[test]
    fn short_ambiguous_inputs_clear_default_threshold() {
        // PRD-canonical short/vague inputs should land >= 0.6 so Adaptive
        // mode shows the card.
        assert!(score_complexity("fix it") >= 0.6, "fix it");
        assert!(score_complexity("explain this") >= 0.6, "explain this");
        assert!(score_complexity("tell me a joke") >= 0.6, "tell me a joke");
    }

    #[test]
    fn well_constrained_inputs_fall_below_threshold() {
        let s = score_complexity(
            "write a formal professional email to my executive team summarising Q3 in bullet points",
        );
        assert!(s < 0.6, "score was {s}, expected < 0.6");
    }

    #[test]
    fn longer_inputs_score_lower_than_short_in_same_domain() {
        let short = score_complexity("fix bug");
        let long = score_complexity(
            "fix the off-by-one bug in the pagination cursor of the list endpoint that breaks when the page size equals the total count",
        );
        assert!(short > long, "short {short} should exceed long {long}");
    }

    #[test]
    fn score_is_clamped_to_unit_interval() {
        for input in [
            "",
            "x",
            "fix it now",
            "tell me a story about the future of computing",
            "compare the architecture of React Server Components against Solid's signals",
        ] {
            let s = score_complexity(input);
            assert!((0.0..=1.0).contains(&s), "{input:?} scored {s}");
        }
    }
}
