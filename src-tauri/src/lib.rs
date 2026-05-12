use std::collections::HashMap;

use tauri::{AppHandle, Manager, Runtime};

mod active_app;
mod app_classifier;
mod clarify;
mod clipboard;
mod command_bar;
mod developer_enhance;
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
        // Single-instance: when the user double-clicks the desktop icon
        // (or relaunches in any way) while PromptForge is already running,
        // bring the existing main window to the foreground instead of
        // spawning a second tray + hotkey owner. The closure receives the
        // CLI args of the second instance, but we don't take args today —
        // we just surface the window.
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            println!("[single-instance] another launch attempted — focusing main window");
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.unminimize();
                let _ = window.show();
                let _ = window.set_focus();
            }
        }))
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
            settings::get_hotkey_enabled,
            settings::set_hotkey_enabled,
            settings::test_connection,
            settings::open_settings,
            settings::get_question_engine_settings,
            settings::save_question_engine_settings,
            settings::get_app_classification_settings,
            settings::save_app_classification_settings,
            settings::get_default_classification_lists,
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
            command_bar::show_command_bar,
            command_bar::hide_command_bar,
            command_bar::open_main_window,
        ])
        .setup(|app| {
            let user_settings = settings::load(app.handle());
            tray::build(app.handle())?;
            // Honour the persisted master toggle on startup. When the
            // user has paused PromptForge, we still build the tray and
            // window but skip global-shortcut registration so the
            // hotkey is genuinely dormant. Flipping the toggle back on
            // from the sidebar re-registers it.
            if user_settings.enabled {
                hotkey::register(app.handle(), &user_settings.hotkey)?;
            } else {
                println!("[hotkey] start-up: master toggle is OFF — not registering");
            }
            install_keep_alive_close_handlers(app.handle());
            // Float the command bar at the top of the primary monitor on
            // startup. The window is created hidden in tauri.conf.json so
            // we get to position it before the first paint.
            if let Err(e) = command_bar::show(app.handle()) {
                println!("[command-bar] startup show failed: {e}");
            }
            // First-run onboarding is handled by the Home screen's banner
            // — no need to spawn a separate Settings window on top of it.
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
    const KEEP_ALIVE_LABELS: &[&str] = &[
        "main",
        "settings",
        "projects",
        "clarify",
        "question-card",
        "status",
        "command-bar",
    ];

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

