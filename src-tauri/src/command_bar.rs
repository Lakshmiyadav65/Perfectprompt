use tauri::{AppHandle, Manager, PhysicalPosition, Runtime};

const WINDOW_LABEL: &str = "command-bar";

/// Pixels from the top of the primary monitor where the floating bar
/// docks. Matches the visual rhythm of native menubar widgets without
/// crowding the title-bar area of the focused app.
const TOP_MARGIN: i32 = 18;

/// Positions the command bar window centered horizontally on the
/// primary monitor and a small fixed margin from the top, then shows
/// it. Used on app startup and whenever the user re-opens it from the
/// tray.
pub fn show<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<()> {
    let Some(window) = app.get_webview_window(WINDOW_LABEL) else {
        println!("[command-bar] window not found");
        return Ok(());
    };

    if let Err(e) = position_top_center(&window) {
        println!("[command-bar] could not position: {e}");
    }

    window.show()?;
    // Don't steal focus — the command bar is informational, the user
    // is presumably mid-task in another app.
    Ok(())
}

pub fn hide<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<()> {
    if let Some(window) = app.get_webview_window(WINDOW_LABEL) {
        window.hide()?;
    }
    Ok(())
}

fn position_top_center<R: Runtime>(
    window: &tauri::WebviewWindow<R>,
) -> tauri::Result<()> {
    let monitor = window
        .current_monitor()?
        .or(window.primary_monitor()?)
        .ok_or_else(|| tauri::Error::Anyhow(anyhow::anyhow!("no monitor available")))?;

    let monitor_size = monitor.size();
    let monitor_pos = monitor.position();
    let scale = monitor.scale_factor();

    let win_size = window.outer_size()?;

    // Center horizontally inside the monitor, anchored TOP_MARGIN dp
    // (logical pixels) from the top edge. The position API takes
    // physical pixels so we scale the margin.
    let x = monitor_pos.x
        + ((monitor_size.width as i32 - win_size.width as i32) / 2);
    let y = monitor_pos.y + ((TOP_MARGIN as f64) * scale) as i32;

    window.set_position(PhysicalPosition::new(x, y))?;
    Ok(())
}

// ---------- Tauri commands ----------

#[tauri::command]
pub fn show_command_bar<R: Runtime>(app: AppHandle<R>) -> Result<(), String> {
    show(&app).map_err(|e| format!("{e}"))
}

#[tauri::command]
pub fn hide_command_bar<R: Runtime>(app: AppHandle<R>) -> Result<(), String> {
    hide(&app).map_err(|e| format!("{e}"))
}

/// Surface the main Home window from the floating bar. Used by the
/// "open" affordance on the bar.
#[tauri::command]
pub fn open_main_window<R: Runtime>(app: AppHandle<R>) -> Result<(), String> {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        window.show().map_err(|e| format!("{e}"))?;
        window.set_focus().map_err(|e| format!("{e}"))?;
    }
    Ok(())
}
