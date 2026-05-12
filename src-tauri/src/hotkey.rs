use std::str::FromStr;
use std::time::Duration;

use anyhow::{anyhow, Result};
use tauri::{AppHandle, Manager, Runtime};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};

use crate::settings::QuestionMode;
use crate::{clipboard, enhance, generation, question_bank, settings, status_window};
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

    if input.trim().is_empty() {
        return Err(anyhow!("captured selection is empty"));
    }

    let input_chars = input.chars().count();
    println!("[capture] {input_chars} chars captured");

    // Store the captured prompt so command handlers can read it.
    {
        let state = app.state::<AppState>();
        let mut pending = state.pending_prompt.lock().unwrap();
        *pending = input.clone();
    }

    // Decide path: silent (capture → enhance → paste) vs. card (question
    // card → enhance → paste). PRD §5.1.1: Adaptive mode compares the
    // complexity score to `question_threshold`; AlwaysAsk and Silent
    // short-circuit that.
    let user_settings = settings::load(app);
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
        run_card_path(app, &input).await
    } else {
        run_silent_path(app, &input).await
    }
}

/// Silent path: no card. Show the status pill, run the enhancement,
/// paste. Used when the user has set `question_mode = Silent` or when
/// the complexity scorer judges the input already specific enough.
async fn run_silent_path<R: Runtime>(app: &AppHandle<R>, input: &str) -> Result<()> {
    let _ = status_window::show_near_cursor(app);

    let enhance_result = enhance::enhance_prompt(app, input).await;
    let _ = status_window::hide(app);

    let enhanced = enhance_result.map_err(|e| anyhow!("enhance failed: {e:#}"))?;

    clipboard::replace_selection(app, &enhanced)
        .await
        .map_err(|e| anyhow!("paste failed: {e}"))?;

    Ok(())
}

/// Card path: fire question generation in parallel with the window open,
/// then show the question-card window. The card's `fetch_question_card_session`
/// command will await the in-flight generation (with a 3s budget) and fall
/// back to the static bank if generation fails or times out.
async fn run_card_path<R: Runtime>(app: &AppHandle<R>, input: &str) -> Result<()> {
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
    println!("[pipeline] question-card window shown and focused");

    Ok(())
}
