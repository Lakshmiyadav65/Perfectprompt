use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tauri::{AppHandle, Emitter, Manager, Runtime};

use std::collections::HashMap;

use crate::app_classifier::AppClassification;
use crate::{auth, enhance, hotkey, AppState};

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

fn default_annotate_hotkey() -> String {
    hotkey::DEFAULT_ANNOTATE_HOTKEY.to_string()
}

fn default_mic_hotkey() -> String {
    hotkey::DEFAULT_MIC_HOTKEY.to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserSettings {
    pub hotkey: String,
    /// Global hotkey for the Visual Annotate overlay. `#[serde(default)]` so
    /// settings.json files predating this feature still deserialise (they get
    /// the `Alt+A` default). Registered alongside `hotkey` in
    /// `hotkey::register`.
    #[serde(default = "default_annotate_hotkey")]
    pub annotate_hotkey: String,
    /// Push-to-talk hotkey for Mic (voice → perfect prompt). Held down while
    /// speaking; the `Shift+`-prefixed variant routes to clean dictation
    /// instead of full enhancement. `#[serde(default)]` so pre-Mic
    /// settings.json files still deserialise (they get the `Alt+M` default).
    /// Registered alongside `hotkey` in `hotkey::register`.
    #[serde(default = "default_mic_hotkey")]
    pub mic_hotkey: String,
    /// **DEPRECATED — DO NOT READ.** Legacy single-user API key from
    /// before per-user scoping landed (v0.4.1 and earlier). Kept on the
    /// struct so existing settings.json files still deserialise, but no
    /// code path reads from it anymore. New code reads
    /// `api_keys[current_user_id]` via `api_key_for_current_user`. A
    /// future migration can drop the field once we're confident no
    /// user has an old settings.json left over.
    #[serde(default)]
    pub api_key: Option<String>,
    /// Per-user Groq API keys, keyed by Supabase user uuid. Populated by
    /// the `save_api_key` command; read by `api_key_for_current_user`.
    /// Multiple users can sign in on the same machine and each have
    /// their own key without leaking across accounts.
    #[serde(default)]
    pub api_keys: HashMap<String, String>,
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
    /// **DEPRECATED — DO NOT READ.** Legacy single-user test result;
    /// replaced by per-user `api_key_test_passed` map.
    #[serde(default)]
    pub last_test_passed: bool,
    /// Per-user "did the test_connection call succeed?" flag, keyed by
    /// Supabase user uuid. Drives the API-key checklist's step 3 across
    /// app restarts on a per-account basis.
    #[serde(default)]
    pub api_key_test_passed: HashMap<String, bool>,
}

impl Default for UserSettings {
    fn default() -> Self {
        Self {
            hotkey: hotkey::DEFAULT_HOTKEY.to_string(),
            annotate_hotkey: hotkey::DEFAULT_ANNOTATE_HOTKEY.to_string(),
            mic_hotkey: hotkey::DEFAULT_MIC_HOTKEY.to_string(),
            api_key: None,
            api_keys: HashMap::new(),
            question_threshold: DEFAULT_QUESTION_THRESHOLD,
            question_mode: QuestionMode::default(),
            remembered_contexts: HashMap::new(),
            app_classification: AppClassificationSettings::default(),
            enabled: default_enabled(),
            last_test_passed: false,
            api_key_test_passed: HashMap::new(),
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

/// Look up the API key for the currently-signed-in user. Returns the
/// raw key string when present, `None` when:
///   - no user is signed in (signed-out / dev mode), OR
///   - the signed-in user hasn't added a key yet
/// Public for `enhance::load_api_key` (BYOK path).
pub(crate) fn api_key_for_current_user<R: Runtime>(app: &AppHandle<R>) -> Option<String> {
    let state = app.state::<AppState>();
    let user_id = auth::current_user_id(state.inner())?;
    let settings = load(app);
    settings
        .api_keys
        .get(&user_id)
        .map(|k| k.trim().to_string())
        .filter(|k| !k.is_empty())
}

#[tauri::command]
pub fn api_key_status<R: Runtime>(app: AppHandle<R>) -> ApiKeyStatus {
    let from_env = std::env::var(ENV_VAR)
        .map(|v| !v.trim().is_empty())
        .unwrap_or(false);

    // Per-user lookup: from_settings is true only when the *current*
    // user has a key. Signed-out users (or dev mode with no Supabase)
    // see from_settings = false and last_test_passed = false, which
    // means the UI shows the "Set up your API key" prompt. A previous
    // user's key on disk is invisible until they sign back in.
    let state = app.state::<AppState>();
    let user_id = auth::current_user_id(state.inner());
    let settings = load(&app);
    let (from_settings, last_test_passed) = match user_id.as_deref() {
        Some(uid) => {
            let has_key = settings
                .api_keys
                .get(uid)
                .map(|k| !k.trim().is_empty())
                .unwrap_or(false);
            let test_ok = settings
                .api_key_test_passed
                .get(uid)
                .copied()
                .unwrap_or(false);
            (has_key, test_ok)
        }
        None => (false, false),
    };

    ApiKeyStatus {
        from_env,
        from_settings,
        last_test_passed,
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
    let state = app.state::<AppState>();
    let user_id = auth::current_user_id(state.inner())
        .ok_or_else(|| "Sign in before saving an API key.".to_string())?;

    let mut settings = load(&app);
    settings
        .api_keys
        .insert(user_id.clone(), trimmed.to_string());
    // A fresh key invalidates the previous test result — the user
    // must re-verify with the new credential.
    settings.api_key_test_passed.remove(&user_id);
    save(&app, &settings).map_err(|e| format!("{e:#}"))?;
    emit_key_changed(&app);
    Ok(())
}

#[tauri::command]
pub fn clear_api_key<R: Runtime>(app: AppHandle<R>) -> std::result::Result<(), String> {
    let state = app.state::<AppState>();
    let user_id = auth::current_user_id(state.inner())
        .ok_or_else(|| "Sign in before clearing the API key.".to_string())?;

    let mut settings = load(&app);
    settings.api_keys.remove(&user_id);
    settings.api_key_test_passed.remove(&user_id);
    save(&app, &settings).map_err(|e| format!("{e:#}"))?;
    emit_key_changed(&app);
    Ok(())
}

/// Broadcast a key-state change so frontend windows can refresh
/// immediately instead of waiting on a polling cycle. Failure to emit
/// is non-fatal — frontends still re-fetch on their next poll. Public
/// (within the crate) so `auth::set_session_token` / `clear_session_token`
/// can trigger a refetch when the signed-in user changes.
pub(crate) fn emit_key_changed<R: Runtime>(app: &AppHandle<R>) {
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

#[tauri::command]
pub fn get_annotate_hotkey<R: Runtime>(app: AppHandle<R>) -> String {
    load(&app).annotate_hotkey
}

/// Persist a new Visual Annotate hotkey and re-register global shortcuts so
/// it takes effect immediately. The combo is parse-validated first (the
/// annotate registration inside `hotkey::register` only logs on a bad combo,
/// so validation has to happen here). We persist before re-registering
/// because `register` reads the annotate combo back from settings.json.
#[tauri::command]
pub fn save_annotate_hotkey<R: Runtime>(
    app: AppHandle<R>,
    combo: String,
) -> std::result::Result<(), String> {
    let trimmed = combo.trim().to_string();
    if trimmed.is_empty() {
        return Err("hotkey combo cannot be empty".into());
    }
    hotkey::validate_combo(&trimmed).map_err(|e| format!("{e:#}"))?;

    let mut settings = load(&app);
    let previous = settings.annotate_hotkey.clone();
    settings.annotate_hotkey = trimmed;
    save(&app, &settings).map_err(|e| format!("{e:#}"))?;

    // Re-register everything (enhance + bypass + annotate). If it fails, roll
    // the annotate combo back so the persisted value matches what's live.
    if let Err(e) = hotkey::reregister(&app, &settings.hotkey) {
        settings.annotate_hotkey = previous;
        let _ = save(&app, &settings);
        let _ = hotkey::reregister(&app, &settings.hotkey);
        return Err(format!("{e:#}"));
    }
    Ok(())
}

#[tauri::command]
pub fn get_mic_hotkey<R: Runtime>(app: AppHandle<R>) -> String {
    load(&app).mic_hotkey
}

/// Persist a new Mic push-to-talk hotkey and re-register global shortcuts so
/// it takes effect immediately. Same shape as `save_annotate_hotkey`: validate
/// the combo, persist before re-registering (because `register` reads the mic
/// combo back from settings.json), and roll back on failure.
#[tauri::command]
pub fn save_mic_hotkey<R: Runtime>(
    app: AppHandle<R>,
    combo: String,
) -> std::result::Result<(), String> {
    let trimmed = combo.trim().to_string();
    if trimmed.is_empty() {
        return Err("hotkey combo cannot be empty".into());
    }
    hotkey::validate_combo(&trimmed).map_err(|e| format!("{e:#}"))?;

    let mut settings = load(&app);
    let previous = settings.mic_hotkey.clone();
    settings.mic_hotkey = trimmed;
    save(&app, &settings).map_err(|e| format!("{e:#}"))?;

    if let Err(e) = hotkey::reregister(&app, &settings.hotkey) {
        settings.mic_hotkey = previous;
        let _ = save(&app, &settings);
        let _ = hotkey::reregister(&app, &settings.hotkey);
        return Err(format!("{e:#}"));
    }
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

/// Write the new test outcome to settings.json under the current
/// user's uuid and broadcast settings:key-changed so the frontend
/// re-fetches api_key_status. No-op when no user is signed in (test
/// results without an account context aren't meaningful).
/// Persistence failure is logged but non-fatal — at worst the user
/// re-tests next launch.
fn persist_test_result<R: Runtime>(app: &AppHandle<R>, passed: bool) {
    let state = app.state::<AppState>();
    let Some(user_id) = auth::current_user_id(state.inner()) else {
        return;
    };
    let mut settings = load(app);
    let prior = settings
        .api_key_test_passed
        .get(&user_id)
        .copied()
        .unwrap_or(false);
    if prior == passed {
        // No-op write — skip the disk hit and the broadcast.
        return;
    }
    settings.api_key_test_passed.insert(user_id, passed);
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
