use std::collections::HashMap;
use std::time::Duration;

use tauri::{AppHandle, Manager, Runtime};

mod clarify;
mod clipboard;
mod enhance;
mod generation;
mod hotkey;
mod projects;
mod question_bank;
mod settings;
mod status_window;
mod tray;
mod updater;

/// Receiver for the in-flight question-generation LLM call. The hotkey
/// pipeline fires the call immediately after capture and stores the
/// receiver here; `clarify::fetch_question_card_session` later awaits it
/// (with timeout) when the card mounts. This is what makes question
/// generation run in parallel with the card render (PRD §6.4).
pub type PendingQuestionsRx = tokio::sync::oneshot::Receiver<
    Result<Vec<question_bank::GeneratedQuestion>, String>,
>;

pub struct AppState {
    pub pending_prompt: std::sync::Mutex<String>,
    pub pending_questions: std::sync::Mutex<Option<PendingQuestionsRx>>,
    /// Session-scoped answer memory (PRD §5.1.6). Keyed by
    /// `ImpactDimension::as_str()` (e.g. "tone", "audience"). Lives only as
    /// long as the tray process; persistent memory is a V2 feature.
    pub remembered_answers: std::sync::Mutex<HashMap<String, String>>,
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    if let Ok(path) = dotenvy::dotenv() {
        println!("[env] loaded {}", path.display());
    }

    tauri::Builder::default()
        .manage(AppState {
            pending_prompt: std::sync::Mutex::new(String::new()),
            pending_questions: std::sync::Mutex::new(None),
            remembered_answers: std::sync::Mutex::new(HashMap::new()),
        })
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            settings::api_key_status,
            settings::save_api_key,
            settings::clear_api_key,
            settings::get_hotkey,
            settings::save_hotkey,
            settings::test_connection,
            settings::open_settings,
            settings::get_question_engine_settings,
            settings::save_question_engine_settings,
            updater::check_for_updates,
            enhance::get_pending_prompt,
            enhance::generate_clarifying_questions,
            enhance::submit_answers_and_enhance,
            projects::list_projects,
            projects::get_active_project,
            projects::add_project,
            projects::update_project,
            projects::delete_project,
            projects::set_active_project,
            projects::read_file_content,
            clarify::fetch_question_card_session,
            clarify::submit_question_card_answers,
            clarify::open_question_card,
            clarify::cancel_question_card,
        ])
        .setup(|app| {
            let user_settings = settings::load(app.handle());
            tray::build(app.handle())?;
            hotkey::register(app.handle(), &user_settings.hotkey)?;
            install_keep_alive_close_handlers(app.handle());
            maybe_show_settings_on_first_run(app.handle(), &user_settings);
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

/// PromptForge is a system-tray app, so the auxiliary windows must outlive
/// the user clicking their X buttons. Tauri 2's default behavior on
/// `CloseRequested` is to *destroy* the WebviewWindow, after which
/// `get_webview_window(label)` returns `None` and the tray menu can no
/// longer re-open it.
///
/// We register a close handler per window that calls `api.prevent_close()`
/// and hides instead. The window remains in the WebviewWindowManager so
/// subsequent shows/focuses succeed. Applies to every non-main window we
/// surface from the tray; the invisible `main` window keeps Tauri's
/// default behavior.
fn install_keep_alive_close_handlers<R: Runtime>(app: &AppHandle<R>) {
    const KEEP_ALIVE_LABELS: &[&str] =
        &["settings", "projects", "clarify", "question-card", "status"];

    for label in KEEP_ALIVE_LABELS {
        let Some(window) = app.get_webview_window(label) else {
            println!("[lifecycle] keep-alive: window {label:?} not found at setup");
            continue;
        };
        let win_for_handler = window.clone();
        let label_for_handler = label.to_string();
        window.on_window_event(move |event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                if let Err(e) = win_for_handler.hide() {
                    println!(
                        "[lifecycle] hide failed for {label_for_handler:?}: {e}"
                    );
                }
            }
        });
    }
}

/// First-run UX: if there is no API key in either the GROQ_API_KEY env var or
/// the saved settings.json, auto-open the Settings window so the user can paste
/// one without having to discover the tray menu.
fn maybe_show_settings_on_first_run<R: Runtime>(
    app: &AppHandle<R>,
    user_settings: &settings::UserSettings,
) {
    let has_env_key = std::env::var("GROQ_API_KEY")
        .map(|v| !v.trim().is_empty())
        .unwrap_or(false);
    let has_saved_key = user_settings
        .api_key
        .as_ref()
        .map(|k| !k.trim().is_empty())
        .unwrap_or(false);

    if has_env_key || has_saved_key {
        return;
    }

    let Some(window) = app.get_webview_window("settings") else {
        return;
    };

    println!("[onboarding] no API key found — auto-showing Settings window");
    let window = window.clone();
    tauri::async_runtime::spawn(async move {
        // Tiny delay so the rest of the app is ready (tray icon visible, etc.)
        tokio::time::sleep(Duration::from_millis(400)).await;
        let _ = window.show();
        let _ = window.set_focus();
    });
}
