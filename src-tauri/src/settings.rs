use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tauri::{AppHandle, Emitter, Manager, Runtime};

use std::collections::HashMap;

use crate::app_classifier::AppClassification;
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

fn default_enabled() -> bool {
    true
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
    /// Context-aware enhancement (active-app routing). Defaults preserve
    /// the legacy "always show questionnaire" behaviour for unknown
    /// surfaces while letting known IDEs/terminals skip the popup.
    #[serde(default)]
    pub app_classification: AppClassificationSettings,
    /// Master enable/disable switch surfaced as the sidebar toggle.
    /// When false, the global shortcut is unregistered and the hotkey
    /// pipeline is dormant. Persisted across launches.
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// Last `test_connection` outcome. Drives the API-key setup
    /// checklist's step 3 across app restarts — without this the step
    /// would reset to pending every launch, forcing the user to
    /// re-test a key that's known to work. Cleared whenever the key
    /// itself changes (save or clear).
    #[serde(default)]
    pub last_test_passed: bool,
}

impl Default for UserSettings {
    fn default() -> Self {
        Self {
            hotkey: hotkey::DEFAULT_HOTKEY.to_string(),
            api_key: None,
            question_threshold: DEFAULT_QUESTION_THRESHOLD,
            question_mode: QuestionMode::default(),
            remembered_contexts: HashMap::new(),
            app_classification: AppClassificationSettings::default(),
            enabled: default_enabled(),
            last_test_passed: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppClassificationSettings {
    /// Extra process names (case-insensitive, includes ".exe") that should
    /// always be treated as developer environments. Merged with the
    /// built-in defaults at classification time.
    #[serde(default)]
    pub developer_apps: Vec<String>,
    /// Extra process names that should always show the questionnaire,
    /// overriding the built-in developer list.
    #[serde(default)]
    pub general_apps: Vec<String>,
    /// What to do when the active app matches neither list. Defaults to
    /// the safer questionnaire path (FR-007).
    #[serde(default = "default_unknown_app_behavior")]
    pub default_unknown_app_behavior: AppClassification,
    /// Whether to inject the active project's description and links into
    /// the developer-direct enhancement call. Off-by-default would force
    /// users to discover the toggle, so we default-on (FR-004).
    #[serde(default = "default_use_project_awareness")]
    pub use_project_awareness_in_developer_apps: bool,
}

fn default_unknown_app_behavior() -> AppClassification {
    AppClassification::General
}

fn default_use_project_awareness() -> bool {
    true
}

impl Default for AppClassificationSettings {
    fn default() -> Self {
        Self {
            developer_apps: Vec::new(),
            general_apps: Vec::new(),
            default_unknown_app_behavior: default_unknown_app_behavior(),
            use_project_awareness_in_developer_apps: default_use_project_awareness(),
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
    /// Persisted result of the most recent `test_connection` call.
    /// Reset by save_api_key / clear_api_key so a fresh key always
    /// requires a re-test before the checklist's step 3 ticks back
    /// to ✓.
    pub last_test_passed: bool,
}

#[tauri::command]
pub fn api_key_status<R: Runtime>(app: AppHandle<R>) -> ApiKeyStatus {
    let from_env = std::env::var(ENV_VAR)
        .map(|v| !v.trim().is_empty())
        .unwrap_or(false);

    let settings = load(&app);
    let from_settings = settings
        .api_key
        .as_ref()
        .map(|k| !k.trim().is_empty())
        .unwrap_or(false);

    ApiKeyStatus {
        from_env,
        from_settings,
        last_test_passed: settings.last_test_passed,
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
    // A fresh key invalidates the previous test result — the user
    // must re-verify with the new credential.
    settings.last_test_passed = false;
    save(&app, &settings).map_err(|e| format!("{e:#}"))?;
    emit_key_changed(&app);
    Ok(())
}

#[tauri::command]
pub fn clear_api_key<R: Runtime>(app: AppHandle<R>) -> std::result::Result<(), String> {
    let mut settings = load(&app);
    settings.api_key = None;
    settings.last_test_passed = false;
    save(&app, &settings).map_err(|e| format!("{e:#}"))?;
    emit_key_changed(&app);
    Ok(())
}

/// Broadcast a key-state change so frontend windows can refresh
/// immediately instead of waiting on a polling cycle. Failure to emit
/// is non-fatal — frontends still re-fetch on their next poll.
fn emit_key_changed<R: Runtime>(app: &AppHandle<R>) {
    let payload = api_key_status(app.clone());
    if let Err(e) = app.emit("settings:key-changed", &payload) {
        eprintln!("[settings] emit settings:key-changed failed: {e}");
    }
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
        // A failed test invalidates any previously-persisted pass.
        persist_test_result(&app, false);
        return ConnectionTest {
            ok: false,
            latency_ms,
            message: format!("Groq returned {status}: {body}"),
        };
    }

    persist_test_result(&app, true);
    ConnectionTest {
        ok: true,
        latency_ms,
        message: "ok".into(),
    }
}

/// Write the new test outcome to settings.json and broadcast
/// settings:key-changed so the frontend re-fetches api_key_status.
/// Persistence failure is logged but non-fatal — at worst the user
/// re-tests next launch.
fn persist_test_result<R: Runtime>(app: &AppHandle<R>, passed: bool) {
    let mut settings = load(app);
    if settings.last_test_passed == passed {
        // No-op write — skip the disk hit and the broadcast.
        return;
    }
    settings.last_test_passed = passed;
    if let Err(e) = save(app, &settings) {
        eprintln!("[settings] persist test result failed: {e:#}");
        return;
    }
    emit_key_changed(app);
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
pub fn get_app_classification_settings<R: Runtime>(
    app: AppHandle<R>,
) -> AppClassificationSettings {
    load(&app).app_classification
}

#[tauri::command]
pub fn save_app_classification_settings<R: Runtime>(
    app: AppHandle<R>,
    payload: AppClassificationSettings,
) -> std::result::Result<(), String> {
    let mut settings = load(&app);
    settings.app_classification = AppClassificationSettings {
        developer_apps: dedupe_lower(payload.developer_apps),
        general_apps: dedupe_lower(payload.general_apps),
        default_unknown_app_behavior: payload.default_unknown_app_behavior,
        use_project_awareness_in_developer_apps: payload
            .use_project_awareness_in_developer_apps,
    };
    save(&app, &settings).map_err(|e| format!("{e:#}"))
}

#[derive(Serialize)]
pub struct DefaultClassificationLists {
    pub developer_apps: Vec<&'static str>,
    pub general_apps: Vec<&'static str>,
}

/// Exposes the built-in lists so the Settings UI can show users which
/// apps are recognised out of the box. Useful guidance when deciding
/// whether to add an override.
#[tauri::command]
pub fn get_default_classification_lists() -> DefaultClassificationLists {
    DefaultClassificationLists {
        developer_apps: crate::app_classifier::DEFAULT_DEVELOPER_APPS.to_vec(),
        general_apps: crate::app_classifier::DEFAULT_GENERAL_APPS.to_vec(),
    }
}

fn dedupe_lower(items: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    items
        .into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .filter(|s| seen.insert(s.to_lowercase()))
        .collect()
}

#[tauri::command]
pub fn get_hotkey_enabled<R: Runtime>(app: AppHandle<R>) -> bool {
    load(&app).enabled
}

/// Master enable/disable toggle. When set to false, all global
/// shortcuts are unregistered so the hotkey stops capturing
/// system-wide. When flipped back to true, the saved hotkey is
/// re-registered. The flag is persisted so the state survives a
/// restart.
#[tauri::command]
pub fn set_hotkey_enabled<R: Runtime>(
    app: AppHandle<R>,
    enabled: bool,
) -> std::result::Result<(), String> {
    let mut settings = load(&app);
    settings.enabled = enabled;
    save(&app, &settings).map_err(|e| format!("{e:#}"))?;

    if enabled {
        hotkey::reregister(&app, &settings.hotkey).map_err(|e| format!("{e:#}"))?;
        println!("[hotkey] enabled — registered {}", settings.hotkey);
    } else {
        hotkey::unregister_all(&app);
    }
    Ok(())
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
