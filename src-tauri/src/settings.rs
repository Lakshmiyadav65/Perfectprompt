use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tauri::{AppHandle, Manager, Runtime};

use std::collections::HashMap;

use crate::{enhance, hotkey};

const SETTINGS_FILE: &str = "settings.json";
const ENV_VAR: &str = "GROQ_API_KEY";

pub const DEFAULT_QUESTION_THRESHOLD: f32 = 0.6;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum QuestionMode {
    Adaptive,
    AlwaysAsk,
    Silent,
}

impl Default for QuestionMode {
    fn default() -> Self {
        QuestionMode::Adaptive
    }
}

fn default_question_threshold() -> f32 {
    DEFAULT_QUESTION_THRESHOLD
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserSettings {
    pub hotkey: String,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default = "default_question_threshold")]
    pub question_threshold: f32,
    #[serde(default)]
    pub question_mode: QuestionMode,
    #[serde(default)]
    pub remembered_contexts: HashMap<String, String>,
}

impl Default for UserSettings {
    fn default() -> Self {
        Self {
            hotkey: hotkey::DEFAULT_HOTKEY.to_string(),
            api_key: None,
            question_threshold: DEFAULT_QUESTION_THRESHOLD,
            question_mode: QuestionMode::default(),
            remembered_contexts: HashMap::new(),
        }
    }
}

fn settings_path<R: Runtime>(app: &AppHandle<R>) -> Result<PathBuf> {
    let dir = app
        .path()
        .app_config_dir()
        .context("could not resolve app config dir")?;
    std::fs::create_dir_all(&dir).context("could not create app config dir")?;
    Ok(dir.join(SETTINGS_FILE))
}

pub fn load<R: Runtime>(app: &AppHandle<R>) -> UserSettings {
    let Ok(path) = settings_path(app) else {
        return UserSettings::default();
    };
    match std::fs::read_to_string(&path) {
        Ok(s) => serde_json::from_str::<UserSettings>(&s).unwrap_or_default(),
        Err(_) => UserSettings::default(),
    }
}

fn save<R: Runtime>(app: &AppHandle<R>, settings: &UserSettings) -> Result<()> {
    let path = settings_path(app)?;
    let json = serde_json::to_string_pretty(settings).context("serialize settings")?;
    std::fs::write(&path, json).context("write settings.json")?;
    Ok(())
}

// ---------- Tauri commands ----------

#[derive(Serialize)]
pub struct ApiKeyStatus {
    pub from_env: bool,
    pub from_settings: bool,
}

#[tauri::command]
pub fn api_key_status<R: Runtime>(app: AppHandle<R>) -> ApiKeyStatus {
    let from_env = std::env::var(ENV_VAR)
        .map(|v| !v.trim().is_empty())
        .unwrap_or(false);

    let from_settings = load(&app)
        .api_key
        .as_ref()
        .map(|k| !k.trim().is_empty())
        .unwrap_or(false);

    ApiKeyStatus {
        from_env,
        from_settings,
    }
}

#[tauri::command]
pub fn save_api_key<R: Runtime>(
    app: AppHandle<R>,
    key: String,
) -> std::result::Result<(), String> {
    let trimmed = key.trim();
    if trimmed.is_empty() {
        return Err("API key cannot be empty".into());
    }
    let mut settings = load(&app);
    settings.api_key = Some(trimmed.to_string());
    save(&app, &settings).map_err(|e| format!("{e:#}"))?;
    Ok(())
}

#[tauri::command]
pub fn clear_api_key<R: Runtime>(app: AppHandle<R>) -> std::result::Result<(), String> {
    let mut settings = load(&app);
    settings.api_key = None;
    save(&app, &settings).map_err(|e| format!("{e:#}"))
}

#[tauri::command]
pub fn get_hotkey<R: Runtime>(app: AppHandle<R>) -> String {
    load(&app).hotkey
}

#[tauri::command]
pub fn save_hotkey<R: Runtime>(
    app: AppHandle<R>,
    combo: String,
) -> std::result::Result<(), String> {
    let trimmed = combo.trim().to_string();
    if trimmed.is_empty() {
        return Err("hotkey combo cannot be empty".into());
    }
    // Validate by re-registering. If parse/registration fails, surface that error
    // and don't persist the bad combo.
    hotkey::reregister(&app, &trimmed).map_err(|e| format!("{e:#}"))?;

    let mut settings = load(&app);
    settings.hotkey = trimmed;
    save(&app, &settings).map_err(|e| format!("{e:#}"))?;
    Ok(())
}

#[derive(Serialize)]
pub struct ConnectionTest {
    pub ok: bool,
    pub latency_ms: u128,
    pub message: String,
}

#[tauri::command]
pub async fn test_connection<R: Runtime>(app: AppHandle<R>) -> ConnectionTest {
    let api_key = match enhance::load_api_key(&app) {
        Ok(k) => k,
        Err(e) => {
            return ConnectionTest {
                ok: false,
                latency_ms: 0,
                message: format!("{e:#}"),
            }
        }
    };

    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            return ConnectionTest {
                ok: false,
                latency_ms: 0,
                message: format!("could not build HTTP client: {e}"),
            }
        }
    };

    let body = json!({
        "model": "llama-3.3-70b-versatile",
        "max_tokens": 8,
        "messages": [{ "role": "user", "content": "ping" }]
    });

    let start = Instant::now();
    let response = match client
        .post("https://api.groq.com/openai/v1/chat/completions")
        .bearer_auth(&api_key)
        .header("content-type", "application/json")
        .json(&body)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            return ConnectionTest {
                ok: false,
                latency_ms: start.elapsed().as_millis(),
                message: format!("network error: {e}"),
            }
        }
    };
    let latency_ms = start.elapsed().as_millis();

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return ConnectionTest {
            ok: false,
            latency_ms,
            message: format!("Groq returned {status}: {body}"),
        };
    }

    ConnectionTest {
        ok: true,
        latency_ms,
        message: "ok".into(),
    }
}

#[derive(Serialize, Deserialize)]
pub struct QuestionEngineSettings {
    pub question_threshold: f32,
    pub question_mode: QuestionMode,
}

#[tauri::command]
pub fn get_question_engine_settings<R: Runtime>(app: AppHandle<R>) -> QuestionEngineSettings {
    let s = load(&app);
    QuestionEngineSettings {
        question_threshold: s.question_threshold,
        question_mode: s.question_mode,
    }
}

#[tauri::command]
pub fn save_question_engine_settings<R: Runtime>(
    app: AppHandle<R>,
    payload: QuestionEngineSettings,
) -> std::result::Result<(), String> {
    if !payload.question_threshold.is_finite()
        || payload.question_threshold < 0.0
        || payload.question_threshold > 1.0
    {
        return Err(format!(
            "question_threshold must be in [0.0, 1.0], got {}",
            payload.question_threshold
        ));
    }
    let mut settings = load(&app);
    settings.question_threshold = payload.question_threshold;
    settings.question_mode = payload.question_mode;
    save(&app, &settings).map_err(|e| format!("{e:#}"))
}

#[tauri::command]
pub fn open_settings<R: Runtime>(app: AppHandle<R>) -> std::result::Result<(), String> {
    let window = app
        .get_webview_window("settings")
        .ok_or_else(|| "settings window not found".to_string())?;
    window.show().map_err(|e| format!("show: {e}"))?;
    window.set_focus().map_err(|e| format!("focus: {e}"))?;
    Ok(())
}
