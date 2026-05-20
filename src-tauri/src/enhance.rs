use std::path::PathBuf;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use tauri::path::BaseDirectory;
use tauri::{AppHandle, Manager, Runtime};

use crate::{clipboard, projects, settings};

const ENV_VAR: &str = "GROQ_API_KEY";
const API_URL: &str = "https://api.groq.com/openai/v1/chat/completions";
const MODEL: &str = "llama-3.3-70b-versatile";
const REQUEST_TIMEOUT_SECS: u64 = 30;

/// Typed Groq client error. Stage D in `pipeline::run` matches on this
/// to surface a rate-limit toast distinct from the generic
/// "Kept original" fallback notification.
///
/// Detection is by HTTP status (429) and, defensively, by the
/// `error.code == "rate_limit_exceeded"` body marker that Groq returns
/// on every rate-limit response. Verified against today's live 429s
/// (see `2026-05-15.jsonl` trace) — Groq does NOT use HTTP 200 with
/// an error body for rate limits, so the status-code check is
/// authoritative.
#[derive(Debug)]
pub enum GroqError {
    /// HTTP 429, or any non-2xx response whose body contains
    /// `"code":"rate_limit_exceeded"`.
    RateLimit { message: String },
    /// Network-level failure: DNS, TCP, TLS, body-read, timeout, etc.
    Network(String),
    /// HTTP 2xx but the response body wasn't a parseable chat
    /// completion (malformed JSON, missing `choices`, empty content).
    InvalidResponse(String),
    /// Anything else — non-2xx with no rate-limit signal (401, 500, etc.).
    Other { status: u16, body: String },
}

impl std::fmt::Display for GroqError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GroqError::RateLimit { message } => {
                write!(f, "groq rate limit: {}", trim_for_log(message, 200))
            }
            GroqError::Network(e) => write!(f, "groq network error: {e}"),
            GroqError::InvalidResponse(e) => write!(f, "groq invalid response: {e}"),
            GroqError::Other { status, body } => {
                write!(f, "groq http {status}: {}", trim_for_log(body, 200))
            }
        }
    }
}

impl std::error::Error for GroqError {}

fn trim_for_log(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
}

/// Pure response classifier. Tested directly — no HTTP, no AppHandle.
/// Caller hands in the status code and body string from a Groq response;
/// gets back either the extracted chat-completion text or a typed error.
///
/// Rate-limit detection: HTTP 429, OR a non-2xx response whose body
/// contains the literal `"code":"rate_limit_exceeded"`. The second
/// branch is defensive — Groq doesn't currently use HTTP 200 with a
/// rate-limit body, but if they ever start, we'll still classify it
/// correctly.
fn classify_response(status: u16, body: &str) -> std::result::Result<String, GroqError> {
    let looks_rate_limited = status == 429 || body.contains(r#""code":"rate_limit_exceeded""#);
    if looks_rate_limited {
        return Err(GroqError::RateLimit {
            message: body.to_string(),
        });
    }
    if !(200..300).contains(&status) {
        return Err(GroqError::Other {
            status,
            body: body.to_string(),
        });
    }
    let parsed: ChatResponse = serde_json::from_str(body)
        .map_err(|e| GroqError::InvalidResponse(format!("JSON parse: {e}")))?;
    parsed
        .choices
        .into_iter()
        .next()
        .and_then(|c| c.message.content)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| GroqError::InvalidResponse("no text content in response".to_string()))
}

/// Single-temperature chat-completion request. The orchestrator at
/// Stage D sets a route-specific value (Code 0.3 / Writing 0.6 /
/// Generic 0.4); `generate_clarifying_questions` uses `ChatRequestJson`
/// instead and remains out of scope for this migration.
#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    max_tokens: u32,
    temperature: f32,
    messages: Vec<Message<'a>>,
}

#[derive(Serialize)]
struct ChatRequestJson<'a> {
    model: &'a str,
    max_tokens: u32,
    messages: Vec<Message<'a>>,
    response_format: ResponseFormat,
}

#[derive(Serialize)]
struct ResponseFormat {
    #[serde(rename = "type")]
    format_type: &'static str,
}

#[derive(Serialize)]
struct Message<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Question {
    pub id: String,
    pub question: String,
    pub options: Vec<String>,
}

#[derive(Deserialize)]
struct QuestionsResponse {
    questions: Vec<Question>,
}

#[derive(Deserialize, Serialize)]
pub struct Answer {
    pub question: String,
    pub answer: String,
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

/// Make a single Groq chat-completion call with explicit knobs. Returns
/// the raw choice content with no trimming, no fence stripping, no
/// validation — Stage E of the pipeline owns all of that. The brief
/// names this the single LLM call of the new architecture; everything
/// else around it is deterministic Rust.
pub async fn call_llm<R: Runtime>(
    app: &AppHandle<R>,
    system_prompt: &str,
    user_message: &str,
    max_tokens: u32,
    temperature: f32,
) -> std::result::Result<String, GroqError> {
    let api_key = load_api_key(app).map_err(|e| GroqError::Network(format!("api key: {e}")))?;

    let body = ChatRequest {
        model: MODEL,
        max_tokens,
        temperature,
        messages: vec![
            Message {
                role: "system",
                content: system_prompt,
            },
            Message {
                role: "user",
                content: user_message,
            },
        ],
    };

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS))
        .build()
        .map_err(|e| GroqError::Network(format!("client build: {e}")))?;

    let response = client
        .post(API_URL)
        .bearer_auth(&api_key)
        .header("content-type", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| GroqError::Network(format!("request failed: {e}")))?;

    let status = response.status().as_u16();
    let body_text = response
        .text()
        .await
        .map_err(|e| GroqError::Network(format!("body read: {e}")))?;
    classify_response(status, &body_text)
}

#[tauri::command]
pub fn get_pending_prompt(app: tauri::AppHandle) -> String {
    let state = app.state::<crate::AppState>();
    let prompt = state.pending_prompt.lock().unwrap();
    println!("[API] get_pending_prompt called, returning {} chars", prompt.len());
    prompt.clone()
}

#[tauri::command]
pub async fn generate_clarifying_questions(app: tauri::AppHandle, prompt: String) -> Result<Vec<Question>, String> {
    async fn inner(app: &tauri::AppHandle, prompt: &str) -> Result<Vec<Question>> {
        println!("[API] generate_clarifying_questions called! Prompt: {:?}", prompt);
        let api_key = load_api_key(app)?;
        println!("[API] Key loaded successfully.");
        let system_prompt = r#"You are an expert prompt engineer. The user has provided a rough prompt. Your task is to generate exactly 5 clarifying questions that will help improve and add detail to the user's prompt. 
Each question must have exactly 3 predefined options for the user to choose from.
You MUST respond with valid JSON in the following format containing exactly one key 'questions':
{
  "questions": [
    {
      "id": "q1",
      "question": "The question text?",
      "options": ["Option 1", "Option 2", "Option 3"]
    }
  ]
}"#;

        // Inject active project context if available
        let active_project = projects::active_project_for(app);
        let full_system_prompt = if let Some(proj) = &active_project {
            let links_text = if proj.links.is_empty() {
                String::new()
            } else {
                format!("\n\nRelevant project links:\n{}", proj.links.iter().map(|l| format!("- {l}")).collect::<Vec<_>>().join("\n"))
            };
            format!(
                "{}\n\nIMPORTANT PROJECT CONTEXT — The user is currently working on a project called \"{}\". Here is detailed information about it:\n\n{}{}\n\nUse this context to make your clarifying questions highly specific and relevant to this project.",
                system_prompt, proj.name, proj.description, links_text
            )
        } else {
            system_prompt.to_string()
        };
        println!("[API] Project context: {}", if active_project.is_some() { "injected" } else { "none" });

        let body = ChatRequestJson {
            model: MODEL,
            max_tokens: 1024,
            messages: vec![
                Message {
                    role: "system",
                    content: &full_system_prompt,
                },
                Message {
                    role: "user",
                    content: prompt,
                },
            ],
            response_format: ResponseFormat { format_type: "json_object" },
        };

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS))
            .build()?;

        println!("[API] Sending POST request to {} ...", API_URL);
        let response = client
            .post(API_URL)
            .bearer_auth(&api_key)
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
            .context("API request failed")?;

        let status = response.status();
        println!("[API] Response status: {}", status);
        if !status.is_success() {
            let err_body = response.text().await.unwrap_or_default();
            println!("[API] Error body: {}", err_body);
            return Err(anyhow!("API returned {status}: {err_body}"));
        }

        let parsed: ChatResponse = response.json().await.context("failed to parse response")?;
        println!("[API] Successfully parsed response JSON");
        let content = parsed
            .choices
            .into_iter()
            .next()
            .and_then(|c| c.message.content)
            .ok_or_else(|| anyhow!("API response had no text content"))?;

        let questions_resp: QuestionsResponse = serde_json::from_str(&content).context("Failed to parse JSON questions")?;
        Ok(questions_resp.questions)
    }

    inner(&app, &prompt).await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn submit_answers_and_enhance(app: tauri::AppHandle, prompt: String, answers: Vec<Answer>) -> Result<(), String> {
    async fn inner(app: &tauri::AppHandle, prompt: &str, answers: &[Answer]) -> Result<()> {
        let answers_text = answers.iter().map(|a| format!("Q: {}\nA: {}", a.question, a.answer)).collect::<Vec<_>>().join("\n\n");
        
        // Include active project context in the enhancement
        let project_context = if let Some(proj) = projects::active_project_for(app) {
            format!("\n\nProject Context ({}):\n{}", proj.name, proj.description)
        } else {
            String::new()
        };
        
        let combined_input = format!("Original Prompt: {}\n\nUser Clarifications:\n{}{}", prompt, answers_text, project_context);

        // Step 9 rewire: route through the new pipeline. The envelope
        // is passed as raw_input; Stages A, C, and E still apply. The
        // older ClarifyPopup invokes this command; the newer
        // QuestionCard uses clarify::submit_question_card_answers.
        // Both paths now share the same orchestrator.
        let pi = crate::pipeline::PipelineInput {
            raw_input: combined_input,
            active_app: "clarify".to_string(),
        };
        let output = crate::pipeline::run(app, pi).await?;

        // Hide window BEFORE pasting so the OS restores focus to the user's target app
        if let Some(window) = app.get_webview_window("clarify") {
            let _ = window.hide();
            // Small delay to let the OS switch focus back to the previous window
            tokio::time::sleep(Duration::from_millis(150)).await;
        }

        clipboard::replace_selection(app, &output.final_text)
            .await
            .map_err(|e| anyhow!("replace failed: {e}"))?;

        if output.used_fallback {
            if let Some(reason) = &output.fallback_reason {
                crate::tray::notify_fallback(app, reason);
            }
        }

        Ok(())
    }

    inner(&app, &prompt, &answers).await.map_err(|e| e.to_string())
}

pub(crate) fn load_api_key<R: Runtime>(app: &AppHandle<R>) -> Result<String> {
    // 1) Env var (.env or shell-set) takes precedence — useful for dev.
    if let Ok(key) = std::env::var(ENV_VAR) {
        let trimmed = key.trim();
        if !trimmed.is_empty() {
            return Ok(trimmed.to_string());
        }
    }

    // 2) Per-user key for the currently-signed-in Supabase account.
    //    Returns None when no user is signed in (signed-out dev mode)
    //    or when the user hasn't saved a key yet. Replaces the legacy
    //    global settings.api_key read so user A's key never leaks to
    //    user B after a sign-out / sign-in.
    if let Some(key) = settings::api_key_for_current_user(app) {
        if !key.is_empty() {
            return Ok(key);
        }
    }

    Err(anyhow!(
        "API key not found. Set {ENV_VAR} in .env, or sign in and paste a key in Settings."
    ))
}

/// Load a bundled prompt file by its bare filename
/// (e.g. `"code-enhancer.md"`). Used by the orchestrator's Stage D to
/// pick the route-specific system prompt, and by `generation.rs` to
/// load the dedicated questions prompt.
pub(crate) fn load_prompt<R: Runtime>(app: &AppHandle<R>, name: &str) -> Result<String> {
    let path = resolve_prompt_path(app, name)?;
    std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read prompt {name} from {}", path.display()))
}

fn resolve_prompt_path<R: Runtime>(app: &AppHandle<R>, name: &str) -> Result<PathBuf> {
    app.path()
        .resolve(format!("prompts/{name}"), BaseDirectory::Resource)
        .with_context(|| format!("failed to resolve prompt resource {name}"))
}

// ─── Tests ────────────────────────────────────────────────────────────
//
// Cover the pure `classify_response` helper. The async `call_llm` is
// not directly tested — it's exercised end-to-end by the eval harness
// (`cargo run --example eval_pass2`) and by manual hotkey runs.

#[cfg(test)]
mod tests {
    use super::*;

    /// Live-captured Groq 429 body shape from `2026-05-15.jsonl`. The
    /// classifier must recognise this even without inspecting the HTTP
    /// status, because the `"code":"rate_limit_exceeded"` marker is the
    /// most reliable signal Groq sends.
    const GROQ_429_BODY: &str = r#"{"error":{"message":"Rate limit reached for model `llama-3.3-70b-versatile` in organization `org_01kret3kcce0rsj8fx3b2g7hbj` service tier `on_demand` on tokens per day (TPD): Limit 100000, Used 98472, Requested 1694. Please try again in 2m23.424s.","type":"tokens","code":"rate_limit_exceeded"}}"#;

    /// Minimal Groq chat-completion success body — the shape
    /// `classify_response` extracts the assistant message from.
    const GROQ_200_BODY: &str = r#"{"choices":[{"message":{"role":"assistant","content":"Refactor the user service to use async/await."}}]}"#;

    #[test]
    fn classify_429_status_returns_rate_limit() {
        let err = classify_response(429, GROQ_429_BODY).expect_err("expected Err");
        match err {
            GroqError::RateLimit { message } => {
                assert!(
                    message.contains("rate_limit_exceeded"),
                    "rate-limit message should preserve Groq's body for diagnosis: {message}"
                );
            }
            other => panic!("expected RateLimit, got {other:?}"),
        }
    }

    #[test]
    fn classify_500_status_returns_other_not_rate_limit() {
        let err = classify_response(500, "Internal server error").expect_err("expected Err");
        match err {
            GroqError::Other { status, body } => {
                assert_eq!(status, 500);
                assert_eq!(body, "Internal server error");
            }
            other => panic!("expected Other, got {other:?}"),
        }
    }

    #[test]
    fn classify_200_with_valid_body_returns_ok_content() {
        let ok = classify_response(200, GROQ_200_BODY).expect("expected Ok");
        assert_eq!(ok, "Refactor the user service to use async/await.");
    }

    /// Defensive case: Groq doesn't currently use HTTP 200 with an
    /// error body, but the classifier should still spot the
    /// rate-limit marker if they ever change. Status is 200 but the
    /// body carries `"code":"rate_limit_exceeded"`.
    #[test]
    fn classify_rate_limit_body_marker_wins_even_on_non_429_status() {
        let err = classify_response(503, GROQ_429_BODY).expect_err("expected Err");
        assert!(
            matches!(err, GroqError::RateLimit { .. }),
            "body marker should classify as RateLimit even when status isn't 429: got {err:?}"
        );
    }

    #[test]
    fn classify_200_with_malformed_body_returns_invalid_response() {
        let err = classify_response(200, "not json {").expect_err("expected Err");
        assert!(
            matches!(err, GroqError::InvalidResponse(_)),
            "malformed body should classify as InvalidResponse: got {err:?}"
        );
    }

    #[test]
    fn classify_200_with_empty_content_returns_invalid_response() {
        let body = r#"{"choices":[{"message":{"role":"assistant","content":""}}]}"#;
        let err = classify_response(200, body).expect_err("expected Err");
        assert!(
            matches!(err, GroqError::InvalidResponse(_)),
            "empty content should classify as InvalidResponse: got {err:?}"
        );
    }
}

