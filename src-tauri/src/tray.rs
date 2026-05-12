use tauri::{
    menu::{Menu, MenuItem},
    tray::{TrayIconBuilder, TrayIconEvent},
    AppHandle, Manager, Runtime,
};

pub fn build<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<()> {
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
        &[&projects_item, &settings_item, &question_card_item, &quit_item],
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
            "projects" => {
                println!("[tray] Projects clicked");
                if let Some(window) = app.get_webview_window("projects") {
                    let _ = window.show();
                    let _ = window.set_focus();
                } else {
                    println!("[tray] projects window not found");
                }
            }
            "settings" => {
                println!("[tray] Settings clicked");
                if let Some(window) = app.get_webview_window("settings") {
                    let _ = window.show();
                    let _ = window.set_focus();
                } else {
                    println!("[tray] settings window not found");
                }
            }
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

