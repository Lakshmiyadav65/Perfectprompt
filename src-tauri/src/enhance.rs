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
const MAX_TOKENS: u32 = 1024;
const REQUEST_TIMEOUT_SECS: u64 = 30;

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    max_tokens: u32,
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

pub async fn enhance_prompt<R: Runtime>(app: &AppHandle<R>, input: &str) -> Result<String> {
    let api_key = load_api_key(app)?;
    let system_prompt = load_meta_prompt(app)?;

    let body = ChatRequest {
        model: MODEL,
        max_tokens: MAX_TOKENS,
        messages: vec![
            Message {
                role: "system",
                content: &system_prompt,
            },
            Message {
                role: "user",
                content: input,
            },
        ],
    };

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS))
        .build()
        .context("could not build HTTP client")?;

    let response = client
        .post(API_URL)
        .bearer_auth(&api_key)
        .header("content-type", "application/json")
        .json(&body)
        .send()
        .await
        .context("API request failed (network or DNS issue)")?;

    let status = response.status();
    if !status.is_success() {
        let err_body = response.text().await.unwrap_or_default();
        return Err(anyhow!("API returned {status}: {err_body}"));
    }

    let parsed: ChatResponse = response
        .json()
        .await
        .context("failed to parse response as JSON")?;

    parsed
        .choices
        .into_iter()
        .next()
        .and_then(|c| c.message.content)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow!("API response had no text content"))
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
        
        let enhanced = enhance_prompt(app, &combined_input).await?;
        
        // Hide window BEFORE pasting so the OS restores focus to the user's target app
        if let Some(window) = app.get_webview_window("clarify") {
            let _ = window.hide();
            // Small delay to let the OS switch focus back to the previous window
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
        
        clipboard::replace_selection(app, &enhanced)
            .await
            .map_err(|e| anyhow!("replace failed: {e}"))?;
            
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

    // 2) Fall back to the API key saved in settings.json by the Settings window.
    if let Some(key) = settings::load(app).api_key {
        let trimmed = key.trim();
        if !trimmed.is_empty() {
            return Ok(trimmed.to_string());
        }
    }

    Err(anyhow!(
        "API key not found. Set {ENV_VAR} in .env, or paste a key in Settings."
    ))
}

fn load_meta_prompt<R: Runtime>(app: &AppHandle<R>) -> Result<String> {
    let path = resolve_prompt_path(app)?;
    std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read meta-prompt from {}", path.display()))
}

fn resolve_prompt_path<R: Runtime>(app: &AppHandle<R>) -> Result<PathBuf> {
    app.path()
        .resolve(
            "prompts/enhancer-system-prompt.md",
            BaseDirectory::Resource,
        )
        .context("failed to resolve meta-prompt resource path")
}
