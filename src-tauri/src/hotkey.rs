use std::str::FromStr;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use tauri::{AppHandle, Manager, Runtime};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};

use crate::active_app::{self, ActiveAppContext};
use crate::app_classifier::{self, AppClassification};
use crate::settings::QuestionMode;
use crate::{
    clipboard, developer_enhance, enhance, foreground_tracker, generation, question_bank,
    settings, status_window,
};
use crate::AppState;

pub const DEFAULT_HOTKEY: &str = "Alt+E";

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
                if let Err(e) = run_capture_pipeline(&app, false).await {
                    println!("[pipeline] capture failed: {e:#}");
                }
            });
        })
        .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("failed to register hotkey: {e}")))?;

    // Shift+hotkey bypass (PRD §5.1.7): skip the question card and go
    // straight to silent enhance + paste, regardless of question_mode.
    // The bypass uses the same chord plus Shift so it's discoverable from
    // the main hotkey. We register best-effort — if the combo can't accept
    // a Shift overlay (rare), the main shortcut still works.
    let bypass_combo = bypass_variant(combo);
    match Shortcut::from_str(&bypass_combo) {
        Ok(bypass_shortcut) => {
            let res = app.global_shortcut().on_shortcut(
                bypass_shortcut,
                |app_handle, _shortcut, event| {
                    if event.state() != ShortcutState::Pressed {
                        return;
                    }
                    println!("[hotkey] bypass pressed (Shift held)");
                    let app = app_handle.clone();
                    tauri::async_runtime::spawn(async move {
                        if let Err(e) = run_capture_pipeline(&app, true).await {
                            println!("[pipeline] bypass capture failed: {e:#}");
                        }
                    });
                },
            );
            match res {
                Ok(()) => println!("[hotkey] registered: {combo} (bypass: {bypass_combo})"),
                Err(e) => println!(
                    "[hotkey] registered {combo}; bypass {bypass_combo} failed: {e}"
                ),
            }
        }
        Err(e) => println!(
            "[hotkey] registered {combo}; could not parse bypass {bypass_combo}: {e}"
        ),
    }

    Ok(())
}

/// Returns the Shift-modified variant of `combo`. We avoid duplicating an
/// existing Shift in the combo, and place the new modifier at the front
/// so it parses cleanly.
fn bypass_variant(combo: &str) -> String {
    if combo
        .split('+')
        .any(|p| p.eq_ignore_ascii_case("shift"))
    {
        combo.to_string()
    } else {
        format!("Shift+{combo}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bypass_variant_prepends_shift_when_absent() {
        assert_eq!(
            bypass_variant("CommandOrControl+Alt+E"),
            "Shift+CommandOrControl+Alt+E"
        );
    }

    #[test]
    fn bypass_variant_is_idempotent_when_shift_already_present() {
        // If the user already bound a shift-inclusive hotkey, don't
        // double-add the modifier — the main and bypass coincide.
        let already = "Shift+CommandOrControl+Alt+E";
        assert_eq!(bypass_variant(already), already);
    }

    #[test]
    fn bypass_variant_case_insensitive_shift_detection() {
        assert_eq!(
            bypass_variant("commandorcontrol+alt+shift+e"),
            "commandorcontrol+alt+shift+e"
        );
    }
}

pub fn reregister<R: Runtime>(app: &AppHandle<R>, combo: &str) -> Result<()> {
    let _ = app.global_shortcut().unregister_all();
    register(app, combo).map_err(|e| anyhow!("{e}"))
}

/// Drop every registered global shortcut. Used by the master "paused"
/// toggle so the hotkey stops capturing system-wide without quitting
/// the app.
pub fn unregister_all<R: Runtime>(app: &AppHandle<R>) {
    let _ = app.global_shortcut().unregister_all();
    println!("[hotkey] all global shortcuts unregistered");
}

/// Tauri command — programmatic trigger for the same pipeline the global
/// hotkey fires. Used by the floating capsule's Enhance icon so the user
/// can run an enhancement without leaving the keyboard-free flow.
///
/// The capsule click steals foreground focus from the user's target app,
/// so before running the pipeline we restore the last tracked user
/// window and let Windows settle. The hotkey path skips this because
/// keypresses don't shift focus.
#[tauri::command]
pub async fn trigger_enhance<R: Runtime>(
    app: AppHandle<R>,
    bypass: Option<bool>,
) -> std::result::Result<(), String> {
    if !foreground_tracker::restore_user_foreground_if_needed() {
        return Err(
            "no recent foreground window to capture from — focus an app first, then try again"
                .into(),
        );
    }
    tokio::time::sleep(std::time::Duration::from_millis(
        foreground_tracker::FOCUS_RESTORE_SETTLE_MS,
    ))
    .await;

    run_capture_pipeline(&app, bypass.unwrap_or(false))
        .await
        .map_err(|e| format!("{e:#}"))
}

async fn run_capture_pipeline<R: Runtime>(
    app: &AppHandle<R>,
    force_bypass: bool,
) -> Result<()> {
    let t0 = Instant::now();

    // Detect the active foreground app BEFORE the clipboard capture so
    // we don't pick up a transient focus change. The synthetic Ctrl+C
    // doesn't move focus on Windows, but reading early also keeps
    // logging in trigger order.
    let active_app = active_app::detect_active_app().unwrap_or_default();

    let input = clipboard::capture_selection(app)
        .await
        .map_err(|e| anyhow!("capture failed: {e}"))?;
    let t_capture = t0.elapsed();

    if input.trim().is_empty() {
        return Err(anyhow!("captured selection is empty"));
    }

    let input_chars = input.chars().count();
    println!("[capture] {input_chars} chars captured in {}ms", t_capture.as_millis());

    // Store the captured prompt so command handlers can read it.
    {
        let state = app.state::<AppState>();
        let mut pending = state.pending_prompt.lock().unwrap();
        *pending = input.clone();
    }

    let user_settings = settings::load(app);

    // Shift+hotkey bypass takes precedence over score / mode / classifier
    // (PRD §5.1.7). It still uses the developer-direct path when the
    // active app is a developer surface so the user gets project context
    // for free.
    if force_bypass {
        println!("[pipeline] Shift bypass — running silent path");
        return run_silent_path(app, &input, &active_app, t0).await;
    }

    // Context-aware routing (FR-002, FR-003): if the foreground app is a
    // developer environment, skip the questionnaire entirely and run the
    // developer-direct enhancement.
    let classification = app_classifier::classify(&active_app, &user_settings.app_classification);
    println!(
        "[pipeline] active app: process={:?} title={:?} → {:?}",
        active_app.process_name, active_app.window_title, classification,
    );

    if matches!(classification, AppClassification::Developer) {
        println!("[pipeline] developer environment — skipping questionnaire");
        return run_developer_path(app, &input, &active_app, t0).await;
    }

    // General environment: keep the existing question-engine routing.
    let score = question_bank::score_complexity(&input);
    let show_card = match user_settings.question_mode {
        QuestionMode::Silent => false,
        QuestionMode::AlwaysAsk => true,
        QuestionMode::Adaptive => score >= user_settings.question_threshold,
    };

    println!(
        "[pipeline] score={:.2} mode={:?} threshold={:.2} → {}",
        score,
        user_settings.question_mode,
        user_settings.question_threshold,
        if show_card { "card" } else { "silent" }
    );

    if show_card {
        run_card_path(app, &input, t0).await
    } else {
        run_silent_path(app, &input, &active_app, t0).await
    }
}

/// Silent path: no card. Show the status pill, run the enhancement,
/// paste. Used when the user has set `question_mode = Silent` or when
/// the complexity scorer judges the input already specific enough. The
/// active-app context is unused by the plain silent path today but is
/// kept on the signature so future routing tweaks don't need a re-wire.
async fn run_silent_path<R: Runtime>(
    app: &AppHandle<R>,
    input: &str,
    _active_app: &ActiveAppContext,
    t0: Instant,
) -> Result<()> {
    let _ = status_window::show_near_cursor(app);

    let t_enhance_start = Instant::now();
    let enhance_result = enhance::enhance_prompt(app, input).await;
    let t_enhance = t_enhance_start.elapsed();
    let _ = status_window::hide(app);

    let enhanced = enhance_result.map_err(|e| anyhow!("enhance failed: {e:#}"))?;

    let t_paste_start = Instant::now();
    clipboard::replace_selection(app, &enhanced)
        .await
        .map_err(|e| anyhow!("paste failed: {e}"))?;
    let t_paste = t_paste_start.elapsed();

    println!(
        "[latency] silent path hotkey→pasted={}ms (enhance={}ms paste={}ms)",
        t0.elapsed().as_millis(),
        t_enhance.as_millis(),
        t_paste.as_millis(),
    );
    Ok(())
}

/// Developer-direct path: skip the questionnaire, inject project
/// awareness context (when enabled), enhance, paste. Triggered when the
/// active foreground app is classified as a developer environment
/// (FR-003). The status pill still appears so the user gets feedback
/// (UI-001 / AC-009).
async fn run_developer_path<R: Runtime>(
    app: &AppHandle<R>,
    input: &str,
    active_app: &ActiveAppContext,
    t0: Instant,
) -> Result<()> {
    let _ = status_window::show_near_cursor(app);

    let t_enhance_start = Instant::now();
    let enhance_result = developer_enhance::enhance_for_developer(app, input, active_app).await;
    let t_enhance = t_enhance_start.elapsed();
    let _ = status_window::hide(app);

    let enhanced = enhance_result.map_err(|e| anyhow!("developer enhance failed: {e:#}"))?;

    let t_paste_start = Instant::now();
    clipboard::replace_selection(app, &enhanced)
        .await
        .map_err(|e| anyhow!("paste failed: {e}"))?;
    let t_paste = t_paste_start.elapsed();

    println!(
        "[latency] developer path hotkey→pasted={}ms (enhance={}ms paste={}ms)",
        t0.elapsed().as_millis(),
        t_enhance.as_millis(),
        t_paste.as_millis(),
    );
    Ok(())
}

/// Card path: fire question generation in parallel with the window open,
/// then show the question-card window. The card's `fetch_question_card_session`
/// command will await the in-flight generation (with a 3s budget) and fall
/// back to the static bank if generation fails or times out.
async fn run_card_path<R: Runtime>(
    app: &AppHandle<R>,
    input: &str,
    t0: Instant,
) -> Result<()> {
    let domain = question_bank::detect_domain(input);

    // Set up the oneshot so the card command can await the LLM result.
    let (tx, rx) = tokio::sync::oneshot::channel();
    {
        let state = app.state::<AppState>();
        let mut guard = state.pending_questions.lock().unwrap();
        *guard = Some(rx);
    }

    // Fire question generation off the critical path.
    let app_for_gen = app.clone();
    let input_for_gen = input.to_string();
    tauri::async_runtime::spawn(async move {
        let result = generation::generate_questions_via_llm(&app_for_gen, &input_for_gen, domain)
            .await
            .map_err(|e| e.to_string());
        match &result {
            Ok(qs) => println!("[generation] LLM returned {} question(s)", qs.len()),
            Err(e) => println!("[generation] LLM call failed: {e}"),
        }
        let _ = tx.send(result);
    });

    let window = app
        .get_webview_window("question-card")
        .ok_or_else(|| anyhow!("question-card window not found"))?;
    let _ = window.eval("window.location.hash = '#/question-card'; window.location.reload();");
    tokio::time::sleep(Duration::from_millis(200)).await;
    window
        .show()
        .map_err(|e| anyhow!("failed to show question-card window: {e}"))?;
    window
        .set_focus()
        .map_err(|e| anyhow!("failed to focus question-card window: {e}"))?;
    let elapsed_ms = t0.elapsed().as_millis();
    println!(
        "[latency] card path hotkey→shown={}ms (PRD §12 target <600ms){}",
        elapsed_ms,
        if elapsed_ms > 600 { "  ⚠ over budget" } else { "" }
    );

    Ok(())
}
