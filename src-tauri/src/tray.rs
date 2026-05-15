use tauri::{
    menu::{Menu, MenuItem},
    tray::{TrayIconBuilder, TrayIconEvent},
    AppHandle, Emitter, Manager, Runtime,
};

pub fn build<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<()> {
    let home_item = MenuItem::with_id(app, "home", "Open PromptForge", true, None::<&str>)?;
    let projects_item = MenuItem::with_id(app, "projects", "Projects", true, None::<&str>)?;
    let settings_item = MenuItem::with_id(app, "settings", "Settings", true, None::<&str>)?;
    let question_card_item = MenuItem::with_id(
        app,
        "question_card_dev",
        "Preview Question Card (no capture)",
        true,
        None::<&str>,
    )?;
    let quit_item = MenuItem::with_id(app, "quit", "Quit PromptForge", true, None::<&str>)?;
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
        .tooltip("PromptForge")
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
    let _ = app.emit("pipeline:fallback", message.to_string());
    println!("[fallback] {message}");
    if let Some(tray) = app.tray_by_id("main-tray") {
        let _ = tray.set_tooltip(Some(message));
        let app2 = app.clone();
        let _ = tauri::async_runtime::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
            if let Some(t) = app2.tray_by_id("main-tray") {
                let _ = t.set_tooltip(Some("PromptForge"));
            }
        });
    }
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
