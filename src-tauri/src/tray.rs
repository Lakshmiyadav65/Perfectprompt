use tauri::{
    menu::{Menu, MenuItem},
    tray::{TrayIconBuilder, TrayIconEvent},
    AppHandle, Emitter, Manager, Runtime,
};

use crate::toast_window;

pub fn build<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<()> {
    let home_item = MenuItem::with_id(app, "home", "Open PerfectPrompt", true, None::<&str>)?;
    let projects_item = MenuItem::with_id(app, "projects", "Projects", true, None::<&str>)?;
    let settings_item = MenuItem::with_id(app, "settings", "Settings", true, None::<&str>)?;
    let question_card_item = MenuItem::with_id(
        app,
        "question_card_dev",
        "Preview Question Card (no capture)",
        true,
        None::<&str>,
    )?;
    let quit_item = MenuItem::with_id(app, "quit", "Quit PerfectPrompt", true, None::<&str>)?;
    let menu = Menu::with_items(
        app,
        &[
            &home_item,
            &projects_item,
            &settings_item,
            &question_card_item,
            &quit_item,
        ],
    )?;

    let icon = app
        .default_window_icon()
        .cloned()
        .ok_or_else(|| tauri::Error::AssetNotFound("default window icon".into()))?;

    TrayIconBuilder::with_id("main-tray")
        .icon(icon)
        .tooltip("PerfectPrompt")
        .menu(&menu)
        .show_menu_on_left_click(true)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "home" => open_main_route(app, "/home"),
            "projects" => open_main_route(app, "/projects"),
            "settings" => open_main_route(app, "/settings"),
            "question_card_dev" => {
                println!("[tray] Question Card (dev preview) clicked");
                if let Some(window) = app.get_webview_window("question-card") {
                    let _ = window.eval(
                        "window.location.hash = '#/question-card'; window.location.reload();",
                    );
                    let _ = window.show();
                    let _ = window.set_focus();
                } else {
                    println!("[tray] question-card window not found");
                }
            }
            "quit" => {
                println!("[tray] Quit clicked");
                app.exit(0);
            }
            _ => {}
        })
        .on_tray_icon_event(|_tray, event| {
            if let TrayIconEvent::Click { .. } = event {
                // Reserved for future use (e.g. toggle status window).
            }
        })
        .build(app)?;

    Ok(())
}

/// Step 10: announce a pipeline fallback to the user.
///
/// V1 surfaces it via two channels because the brief disallows
/// changes to the React frontend and doesn't authorise a new
/// notification dependency:
///   1. a `pipeline:fallback` Tauri event (a future frontend toast
///      can listen without us shipping one today);
///   2. a transient tray-icon tooltip update (~1.5 s) so the hover
///      surface reflects the most recent fallback reason.
///
/// Also logs to stderr. Never blocks the calling pipeline.
pub fn notify_fallback<R: Runtime>(app: &AppHandle<R>, message: &str) {
    // 1) Frontend `pipeline:fallback` event — the in-app Toast
    //    component listens for this and renders the actual visible
    //    notification. The Rust side just owns showing the borderless
    //    toast window; the React side owns the message text +
    //    animations + auto-dismiss.
    let _ = app.emit("pipeline:fallback", message.to_string());
    // 2) Stderr trace (dev-time observability).
    println!("[fallback] {message}");
    // 3) Show the app-styled toast window. The OS-native
    //    `tauri-plugin-notification` path is intentionally NOT used
    //    here — it produced a generic Windows Action Center toast
    //    that didn't match the PerfectPrompt dark UI. The in-app toast
    //    matches the rest of the app's visual language.
    if let Err(e) = toast_window::show(app) {
        eprintln!("[fallback] toast window show failed: {e:#}");
    }
    // 4) Tray-tooltip surrogate — kept for hover-discoverability and
    //    because some users may have the toast window blocked by
    //    full-screen apps / desktop policies.
    if let Some(tray) = app.tray_by_id("main-tray") {
        let _ = tray.set_tooltip(Some(message));
        let app2 = app.clone();
        let _ = tauri::async_runtime::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
            if let Some(t) = app2.tray_by_id("main-tray") {
                let _ = t.set_tooltip(Some("PerfectPrompt"));
            }
        });
    }
}

/// Canonical text for the Groq rate-limit notification. Exposed so
/// callers outside the pipeline (and unit tests) can pin the exact
/// wording.
///
/// In the current production flow the pipeline's `friendly_reason`
/// maps `"groq_rate_limit"` to this same string, so the live toast
/// is fired indirectly through `notify_fallback`. This constant is
/// the canonical source for that text — don't drift it out of sync
/// with `pipeline::friendly_reason`.
#[allow(dead_code)] // public convenience surface; see doc note above.
pub const RATE_LIMIT_MESSAGE: &str = "Your API limit has been reached.";

/// Sibling to [`notify_fallback`] specifically for Groq rate-limit
/// hits. Uses the same tray-tooltip + `pipeline:fallback` event
/// mechanism — only the body text differs, so the user can tell
/// "Groq is throttling us" apart from "your input was bad" without
/// us shipping a new notification dependency.
///
/// The pipeline's [`crate::pipeline::run`] already surfaces this
/// text via `friendly_reason("groq_rate_limit")` → the
/// hotkey/clarify callers' existing `notify_fallback` invocation,
/// so the live flow does NOT need this function. Provided for code
/// paths outside the pipeline that want to fire the rate-limit
/// toast directly (manual recovery, future retry logic, etc.).
#[allow(dead_code)] // public convenience surface; see doc note above.
pub fn notify_rate_limit<R: Runtime>(app: &AppHandle<R>) {
    notify_fallback(app, RATE_LIMIT_MESSAGE);
}

/// Show the main window and navigate it to the requested hash route. The
/// shell renders Home / Projects / Settings inside the main window so the
/// tray no longer needs to spawn separate windows for each.
fn open_main_route<R: Runtime>(app: &AppHandle<R>, route: &str) {
    let Some(window) = app.get_webview_window("main") else {
        println!("[tray] main window not found");
        return;
    };
    let script = format!("window.location.hash = '#{route}';");
    let _ = window.eval(&script);
    let _ = window.show();
    let _ = window.set_focus();

    // Opening the app re-surfaces the floating command bar only when the
    // persisted toggle is ON. When paused, the bar stays hidden — the
    // user flips the sidebar toggle from the main window to bring it back.
    if crate::settings::load(app).enabled {
        if let Err(e) = crate::command_bar::show(app) {
            println!("[tray] command bar show failed: {e}");
        }
    }
}
