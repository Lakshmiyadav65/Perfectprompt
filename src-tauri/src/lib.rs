use std::collections::HashMap;

use tauri::{AppHandle, Emitter, Manager, Runtime};

mod active_app;
mod app_classifier;
mod auth;
mod cache;
mod clarify;
mod clipboard;
mod command_bar;
mod enhance;
mod enhancement_history;
mod foreground_tracker;
mod generation;
pub mod github_analyze;
mod hosted;
mod hotkey;
pub mod intake;
pub mod pipeline;
mod project_scan;
pub mod projects;
mod question_bank;
pub mod router;
mod settings;
mod status_window;
mod toast_window;
mod trace;
mod tray;
mod updater;
pub mod usage;
pub mod validate;

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
    /// Stage B of the enhancement pipeline — keyed on the Stage A
    /// fingerprint, so identical input + active-app skips the LLM.
    /// Wired in at Step 8 (pipeline::run()).
    pub cache: cache::EnhancementCache,
    /// Supabase session JWT pushed from JS after sign-in. When `Some`,
    /// the pipeline routes through the hosted /enhance edge function
    /// instead of calling Groq directly. JS owns the OAuth dance and
    /// refreshes the token on its side.
    pub session_token: std::sync::Mutex<Option<String>>,
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
            cache: cache::EnhancementCache::default(),
            session_token: std::sync::Mutex::new(None),
        })
        .manage(usage::UsageState::new())
        // Single-instance: when the user double-clicks the desktop icon
        // (or relaunches in any way) while PerfectPrompt is already running,
        // bring the existing main window to the foreground instead of
        // spawning a second tray + hotkey owner. The closure receives the
        // CLI args of the second instance. On Windows, OAuth deep-link
        // callbacks (`perfectprompt://auth/callback?code=...`) arrive as
        // argv on a fresh process — the OS launches a new exe and hands
        // the URL in. Without the bridge below, single-instance would
        // discard those URLs before the deep-link plugin ever saw them.
        .plugin(tauri_plugin_single_instance::init(|app, argv, _cwd| {
            println!("[single-instance] another launch attempted — surfacing app");
            // Forward any deep-link URLs in argv to the JS layer. The
            // useAuth hook listens for `deep-link-from-argv` and runs the
            // same handleCallbackUrl that the deep-link plugin would.
            for arg in argv.iter().skip(1) {
                if arg.starts_with("perfectprompt://") {
                    println!("[single-instance] forwarding deep-link arg: {arg}");
                    if let Err(e) = app.emit("deep-link-from-argv", arg) {
                        eprintln!("[single-instance] emit deep-link-from-argv failed: {e}");
                    }
                }
            }
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.unminimize();
                let _ = window.show();
                let _ = window.set_focus();
            }
            // Re-surface the floating command bar only if the user's
            // persisted toggle is ON. When paused, opening the app is the
            // path the user takes to flip the toggle back on themselves.
            if settings::load(app).enabled {
                if let Err(e) = command_bar::show(app) {
                    println!("[single-instance] command bar show failed: {e}");
                }
            }
        }))
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_deep_link::init())
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
            projects::clear_active_project,
            projects::refresh_project_context,
            projects::get_cached_context_timestamp,
            projects::read_file_content,
            clarify::fetch_question_card_session,
            clarify::submit_question_card_answers,
            clarify::open_question_card,
            clarify::cancel_question_card,
            command_bar::show_command_bar,
            command_bar::hide_command_bar,
            command_bar::open_main_window,
            github_analyze::analyze_github_repo,
            hotkey::trigger_enhance,
            auth::set_session_token,
            auth::clear_session_token,
            auth::get_auth_status,
            enhancement_history::list_enhancements,
            enhancement_history::delete_enhancement,
            usage::get_usage_state,
        ])
        .setup(|app| {
            // Register the perfectprompt:// URL scheme at runtime so OAuth
            // callbacks survive a dev launch. Production installers register
            // the scheme via the deep-link plugin's bundle metadata.
            #[cfg(any(windows, target_os = "linux"))]
            {
                use tauri_plugin_deep_link::DeepLinkExt;
                if let Err(e) = app.deep_link().register("perfectprompt") {
                    println!("[deep-link] register failed: {e}");
                }
            }
            let user_settings = settings::load(app.handle());

            // One-shot cleanup of the user-facing enhancement history.
            // Removes content-duplicate entries that may have made it
            // to disk before the in-append dedup landed, and rewrites
            // the file in chronological order.
            enhancement_history::dedupe_and_sort_file(app.handle());

            tray::build(app.handle())?;
            // Honour the persisted master toggle on startup. When the
            // user has paused PerfectPrompt, we still build the tray and
            // window but skip global-shortcut registration so the
            // hotkey is genuinely dormant. Flipping the toggle back on
            // from the sidebar re-registers it.
            if user_settings.enabled {
                hotkey::register(app.handle(), &user_settings.hotkey)?;
            } else {
                println!("[hotkey] start-up: master toggle is OFF — not registering");
            }
            install_keep_alive_close_handlers(app.handle());
            // Start the foreground tracker so the capsule's Enhance icon
            // can restore the user's prior window before capturing.
            foreground_tracker::spawn();
            // Float the command bar at the top of the primary monitor on
            // startup, but only if the user's persisted toggle is ON. When
            // paused, the bar stays hidden until the user flips the toggle
            // back on from the sidebar.
            if user_settings.enabled {
                if let Err(e) = command_bar::show(app.handle()) {
                    println!("[command-bar] startup show failed: {e}");
                }
            }
            // First-run onboarding is handled by the Home screen's banner
            // — no need to spawn a separate Settings window on top of it.
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

/// PerfectPrompt is a system-tray app, so the auxiliary windows must outlive
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
        "toast",
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

