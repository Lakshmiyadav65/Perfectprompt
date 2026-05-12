use std::collections::HashSet;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Runtime};

use crate::enhance::load_api_key;
use crate::question_bank::{Domain, GeneratedQuestion, ImpactDimension, QuestionType};

const API_URL: &str = "https://api.groq.com/openai/v1/chat/completions";

/// Faster, cheaper model for the classification-style question-generation
/// task (PRD §13 Q4). The enhancement call keeps the 70B model.
const MODEL: &str = "llama-3.1-8b-instant";

const MAX_TOKENS: u32 = 768;

/// PRD §6.5: question generation must give up at 3s so the card can fall
/// back to the static bank.
pub const GENERATION_TIMEOUT_SECS: u64 = 3;

/// Hard cap on questions returned. PRD §7.1: "Never more than 4 questions."
pub const MAX_QUESTIONS: usize = 4;

/// PRD §5.1.2 minimum threshold — if the LLM returns fewer valid questions
/// than this, the card falls back to the static domain bank.
pub const MIN_VALID_QUESTIONS: usize = 2;

const SYSTEM_PROMPT: &str = r#"You generate clarifying questions for a prompt-enhancement tool.

The user's message contains a rough prompt they're about to send to an LLM. Your job is to figure out what's missing — what would most improve the quality of the enhanced prompt — and ask 2 to 4 short questions to surface that context.

Respond with a single JSON object: {"questions": [...]}.

Each item in the questions array must match this schema:
{
  "id": "q1",
  "question": "Who is this for?",
  "type": "chips" | "single_select" | "multi_select" | "free_text",
  "options": ["Manager", "Client", "Team"],
  "placeholder": null,
  "impact_dimension": "tone" | "audience" | "goal" | "constraints" | "format" | "length" | "domain" | "other",
  "required": false
}

Rules:
- Output ONLY the JSON object. No preamble. No markdown fences. No explanation.
- Generate 2 to 4 questions, each targeting a DISTINCT impact_dimension.
- Do not ask questions that are already answered in the input.
- Prefer "chips" or "single_select" with 3-5 short option labels (1-3 words each).
- Use "free_text" only when no fixed options would make sense.
- Question text must be short (max ~50 chars) and conversational, not robotic.
- If the input is already specific and well-constrained, return {"questions": []}.
"#;

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    max_tokens: u32,
    messages: Vec<Message<'a>>,
    response_format: ResponseFormat,
    temperature: f32,
}

#[derive(Serialize)]
struct Message<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Serialize)]
struct ResponseFormat {
    #[serde(rename = "type")]
    format_type: &'static str,
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
struct QuestionsEnvelope {
    #[serde(default)]
    questions: Vec<RawQuestion>,
}

#[derive(Deserialize)]
struct RawQuestion {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    question: String,
    #[serde(rename = "type", default)]
    question_type: Option<String>,
    #[serde(default)]
    options: Vec<String>,
    #[serde(default)]
    placeholder: Option<String>,
    #[serde(default)]
    impact_dimension: Option<String>,
    #[serde(default)]
    required: bool,
}

/// Calls the small Groq model to generate 2-4 clarifying questions tailored
/// to the captured input. Returns the validated, deduplicated, truncated
/// list — callers are expected to fall back to the static bank if fewer
/// than `MIN_VALID_QUESTIONS` come back.
pub async fn generate_questions_via_llm<R: Runtime>(
    app: &AppHandle<R>,
    input: &str,
    domain: Domain,
) -> Result<Vec<GeneratedQuestion>> {
    let api_key = load_api_key(app)?;

    let user_message = format!(
        "[GENERATE_QUESTIONS]\nDetected domain: {:?}\n\nInput:\n{}",
        domain, input
    );

    let body = ChatRequest {
        model: MODEL,
        max_tokens: MAX_TOKENS,
        temperature: 0.2,
        messages: vec![
            Message {
                role: "system",
                content: SYSTEM_PROMPT,
            },
            Message {
                role: "user",
                content: &user_message,
            },
        ],
        response_format: ResponseFormat {
            format_type: "json_object",
        },
    };

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(GENERATION_TIMEOUT_SECS + 1))
        .build()
        .context("could not build HTTP client")?;

    let response = client
        .post(API_URL)
        .bearer_auth(&api_key)
        .header("content-type", "application/json")
        .json(&body)
        .send()
        .await
        .context("question-generation request failed")?;

    let status = response.status();
    if !status.is_success() {
        let err_body = response.text().await.unwrap_or_default();
        return Err(anyhow!(
            "question-generation API returned {status}: {err_body}"
        ));
    }

    let parsed: ChatResponse = response
        .json()
        .await
        .context("question-generation response was not JSON")?;

    let raw_content = parsed
        .choices
        .into_iter()
        .next()
        .and_then(|c| c.message.content)
        .ok_or_else(|| anyhow!("question-generation response had no content"))?;

    let envelope: QuestionsEnvelope = serde_json::from_str(&raw_content)
        .context("question-generation output was not a JSON object with a questions array")?;

    Ok(validate_and_dedupe(envelope.questions))
}

fn validate_and_dedupe(raw: Vec<RawQuestion>) -> Vec<GeneratedQuestion> {
    let mut seen: HashSet<ImpactDimension> = HashSet::new();
    let mut out: Vec<GeneratedQuestion> = Vec::new();

    for (idx, q) in raw.into_iter().enumerate() {
        let question_text = q.question.trim().to_string();
        if question_text.is_empty() {
            continue;
        }

        let qt = parse_question_type(q.question_type.as_deref().unwrap_or("chips"))
            .unwrap_or(QuestionType::Chips);
        let dim = parse_impact_dimension(q.impact_dimension.as_deref().unwrap_or("other"));

        // PRD: each question targets a DISTINCT impact dimension. Drop dupes.
        if !seen.insert(dim) {
            continue;
        }

        // Option-based widgets must have options. Otherwise demote to free_text.
        let (final_type, options) = if matches!(
            qt,
            QuestionType::Chips | QuestionType::SingleSelect | QuestionType::MultiSelect
        ) {
            if q.options.is_empty() {
                (QuestionType::FreeText, Vec::new())
            } else {
                (qt, q.options)
            }
        } else {
            (qt, q.options)
        };

        out.push(GeneratedQuestion {
            id: q
                .id
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| format!("llm_q{}", idx + 1)),
            question: question_text,
            question_type: final_type,
            options,
            placeholder: q.placeholder.filter(|s| !s.trim().is_empty()),
            impact_dimension: dim,
            required: q.required,
        });

        if out.len() >= MAX_QUESTIONS {
            break;
        }
    }

    out
}

fn parse_question_type(s: &str) -> Option<QuestionType> {
    match s.trim().to_lowercase().as_str() {
        "chips" => Some(QuestionType::Chips),
        "single_select" | "single-select" | "singleselect" | "select" => {
            Some(QuestionType::SingleSelect)
        }
        "multi_select" | "multi-select" | "multiselect" | "checkboxes" => {
            Some(QuestionType::MultiSelect)
        }
        "free_text" | "free-text" | "freetext" | "text" => Some(QuestionType::FreeText),
        _ => None,
    }
}

fn parse_impact_dimension(s: &str) -> ImpactDimension {
    match s.trim().to_lowercase().as_str() {
        "tone" => ImpactDimension::Tone,
        "audience" => ImpactDimension::Audience,
        "goal" => ImpactDimension::Goal,
        "constraints" | "constraint" => ImpactDimension::Constraints,
        "format" => ImpactDimension::Format,
        "length" => ImpactDimension::Length,
        "domain" => ImpactDimension::Domain,
        _ => ImpactDimension::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_raw(
        question: &str,
        ty: &str,
        options: &[&str],
        dim: &str,
    ) -> RawQuestion {
        RawQuestion {
            id: None,
            question: question.to_string(),
            question_type: Some(ty.to_string()),
            options: options.iter().map(|s| s.to_string()).collect(),
            placeholder: None,
            impact_dimension: Some(dim.to_string()),
            required: false,
        }
    }

    #[test]
    fn dedupes_by_impact_dimension() {
        let raw = vec![
            make_raw("Who is this for?", "chips", &["Manager", "Client"], "audience"),
            make_raw("Who reads this?", "chips", &["Team", "Public"], "audience"),
            make_raw("What tone?", "chips", &["Formal", "Casual"], "tone"),
        ];
        let out = validate_and_dedupe(raw);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].impact_dimension, ImpactDimension::Audience);
        assert_eq!(out[1].impact_dimension, ImpactDimension::Tone);
    }

    #[test]
    fn drops_empty_question_text() {
        let raw = vec![
            make_raw("", "chips", &["a", "b"], "tone"),
            make_raw("What tone?", "chips", &["Formal", "Casual"], "tone"),
        ];
        let out = validate_and_dedupe(raw);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].question, "What tone?");
    }

    #[test]
    fn chips_without_options_demoted_to_free_text() {
        let raw = vec![make_raw("Anything else?", "chips", &[], "other")];
        let out = validate_and_dedupe(raw);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].question_type, QuestionType::FreeText);
    }

    #[test]
    fn caps_at_max_questions() {
        let raw = vec![
            make_raw("Q1", "chips", &["a"], "tone"),
            make_raw("Q2", "chips", &["a"], "audience"),
            make_raw("Q3", "chips", &["a"], "goal"),
            make_raw("Q4", "chips", &["a"], "format"),
            make_raw("Q5", "chips", &["a"], "length"),
            make_raw("Q6", "chips", &["a"], "domain"),
        ];
        let out = validate_and_dedupe(raw);
        assert_eq!(out.len(), MAX_QUESTIONS);
    }

    #[test]
    fn unknown_dimension_falls_back_to_other() {
        let raw = vec![make_raw("?", "chips", &["a"], "vibe")];
        let out = validate_and_dedupe(raw);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].impact_dimension, ImpactDimension::Other);
    }

    #[test]
    fn unknown_question_type_defaults_to_chips() {
        let raw = vec![make_raw("?", "radio", &["a", "b"], "tone")];
        let out = validate_and_dedupe(raw);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].question_type, QuestionType::Chips);
    }

    #[test]
    fn fills_in_missing_id() {
        let raw = vec![RawQuestion {
            id: None,
            question: "Who?".into(),
            question_type: Some("chips".into()),
            options: vec!["A".into()],
            placeholder: None,
            impact_dimension: Some("audience".into()),
            required: false,
        }];
        let out = validate_and_dedupe(raw);
        assert_eq!(out.len(), 1);
        assert!(!out[0].id.is_empty());
    }
}
