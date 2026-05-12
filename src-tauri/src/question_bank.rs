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

/// Lightweight keyword classifier — implemented in Task 2 (Phase 1: Static flow).
/// Returns `Generic` for now so the question card can render with the generic bank.
pub fn detect_domain(_input: &str) -> Domain {
    Domain::Generic
}

/// Static domain-aware question bank — implemented in Task 2 using the full
/// catalogue from the PRD Appendix (Section 14). Empty for scaffolding.
pub fn static_questions_for(_domain: Domain) -> Vec<GeneratedQuestion> {
    Vec::new()
}
