use std::str::FromStr;

use anyhow::{anyhow, Result};
use tauri::{AppHandle, Manager, Runtime};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};

use crate::clipboard;
use crate::AppState;

pub const DEFAULT_HOTKEY: &str = "CommandOrControl+Alt+E";

pub fn register<R: Runtime>(app: &AppHandle<R>, combo: &str) -> tauri::Result<()> {
    let shortcut = Shortcut::from_str(combo)
        .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("invalid hotkey {combo:?}: {e}")))?;

    app.global_shortcut()
        .on_shortcut(shortcut, |app_handle, _shortcut, event| {
            if event.state() != ShortcutState::Pressed {
                return;
            }
            println!("[hotkey] pressed");
            let app = app_handle.clone();
            tauri::async_runtime::spawn(async move {
                if let Err(e) = run_capture_pipeline(&app).await {
                    println!("[pipeline] capture failed: {e:#}");
                }
            });
        })
        .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("failed to register hotkey: {e}")))?;

    println!("[hotkey] registered: {combo}");
    Ok(())
}

pub fn reregister<R: Runtime>(app: &AppHandle<R>, combo: &str) -> Result<()> {
    let _ = app.global_shortcut().unregister_all();
    register(app, combo).map_err(|e| anyhow!("{e}"))
}

async fn run_capture_pipeline<R: Runtime>(app: &AppHandle<R>) -> Result<()> {
    let input = clipboard::capture_selection(app)
        .await
        .map_err(|e| anyhow!("capture failed: {e}"))?;
    let input_chars = input.chars().count();
    println!("[capture] {input_chars} chars captured");

    if input.trim().is_empty() {
        return Err(anyhow!("captured selection is empty"));
    }

    // Store the prompt in shared state so the frontend can fetch it
    let state = app.state::<AppState>();
    {
        let mut pending = state.pending_prompt.lock().unwrap();
        *pending = input.clone();
        println!("[pipeline] stored prompt in state ({} chars)", input.len());
    }

    // Open the Smart Question Engine card (Phase 1+). The PRD makes this the
    // default destination for the hotkey; the legacy clarify window remains
    // reachable via its hash route but is no longer hotkey-triggered.
    if let Some(window) = app.get_webview_window("question-card") {
        let _ = window.eval(
            "window.location.hash = '#/question-card'; window.location.reload();",
        );
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        window
            .show()
            .map_err(|e| anyhow!("failed to show question-card window: {e}"))?;
        window
            .set_focus()
            .map_err(|e| anyhow!("failed to focus question-card window: {e}"))?;
        println!("[pipeline] question-card window shown and focused");
    } else {
        return Err(anyhow!("question-card window not found"));
    }

    Ok(())
}
