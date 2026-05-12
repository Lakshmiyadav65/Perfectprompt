use tauri::{
    menu::{Menu, MenuItem},
    tray::{TrayIconBuilder, TrayIconEvent},
    AppHandle, Manager, Runtime,
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

    // Opening the app re-surfaces the floating command bar too.
    // Single entry point — user never needs to think about the bar
    // separately from the app itself.
    if let Err(e) = crate::command_bar::show(app) {
        println!("[tray] command bar show failed: {e}");
    }
}
