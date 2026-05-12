use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, Runtime};

use crate::question_bank::{
    detect_domain, static_questions_for, GeneratedQuestion, ImpactDimension,
};
use crate::{clipboard, enhance};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuestionAnswer {
    pub question_id: String,
    pub impact_dimension: ImpactDimension,
    pub value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuestionSession {
    pub original_input: String,
    pub questions: Vec<GeneratedQuestion>,
    #[serde(default)]
    pub answers: Vec<QuestionAnswer>,
}

/// Reads the captured input from `AppState.pending_prompt` (populated by the
/// hotkey pipeline), runs the keyword classifier, and returns the matching
/// static question set. Task 3 will layer LLM-generated questions on top of
/// this same shape.
#[tauri::command]
pub fn fetch_question_card_session<R: Runtime>(app: AppHandle<R>) -> QuestionSession {
    let state = app.state::<crate::AppState>();
    let original = state.pending_prompt.lock().unwrap().clone();

    let domain = detect_domain(&original);
    let questions = static_questions_for(domain);
    println!(
        "[clarify] fetch session — domain={:?}, {} question(s)",
        domain,
        questions.len()
    );

    QuestionSession {
        original_input: original,
        questions,
        answers: Vec::new(),
    }
}

/// Receives the user's answers, builds the [CONTEXT] block, runs the
/// enhancement LLM call, hides the question-card window, and pastes the
/// enhanced text in place of the user's original selection.
#[tauri::command]
pub async fn submit_question_card_answers(
    app: AppHandle,
    answers: Vec<QuestionAnswer>,
) -> Result<(), String> {
    println!(
        "[clarify] submit_question_card_answers: {} answer(s)",
        answers.len()
    );

    let original = {
        let state = app.state::<crate::AppState>();
        let pending = state.pending_prompt.lock().unwrap();
        pending.clone()
    };

    if original.trim().is_empty() {
        return Err("no captured input to enhance — press the hotkey first".into());
    }

    let combined_input = assemble_context(&original, &answers);
    println!(
        "[clarify] assembled context block ({} chars including original)",
        combined_input.len()
    );

    let enhanced = enhance::enhance_prompt(&app, &combined_input)
        .await
        .map_err(|e| format!("enhancement failed: {e:#}"))?;

    if let Some(window) = app.get_webview_window("question-card") {
        let _ = window.hide();
        // Brief settle so the OS restores focus to the user's target app
        // before we synthesize Ctrl+V.
        tokio::time::sleep(Duration::from_millis(150)).await;
    }

    clipboard::replace_selection(&app, &enhanced)
        .await
        .map_err(|e| format!("paste failed: {e}"))?;

    Ok(())
}

/// Shows the question-card window. Used by the tray dev menu and by the
/// hotkey pipeline.
#[tauri::command]
pub fn open_question_card<R: Runtime>(app: AppHandle<R>) -> Result<(), String> {
    let window = app
        .get_webview_window("question-card")
        .ok_or_else(|| "question-card window not found".to_string())?;
    window.show().map_err(|e| format!("show: {e}"))?;
    window.set_focus().map_err(|e| format!("focus: {e}"))?;
    Ok(())
}

/// Cancels the card and hides the window. The clipboard is not touched
/// during card display, so there is nothing to restore.
#[tauri::command]
pub fn cancel_question_card<R: Runtime>(app: AppHandle<R>) -> Result<(), String> {
    if let Some(window) = app.get_webview_window("question-card") {
        window.hide().map_err(|e| format!("hide: {e}"))?;
    }
    Ok(())
}

/// Builds the `[CONTEXT]` block per PRD §5.1.5, with the user's original
/// captured input and any answers they supplied. Empty/whitespace-only
/// answers are dropped. The trailing instruction line is what the existing
/// meta-prompt expects to act on.
pub fn assemble_context(original: &str, answers: &[QuestionAnswer]) -> String {
    let mut out = String::new();
    out.push_str("[CONTEXT]\n");
    out.push_str(&format!("Original input: {}\n", original.trim()));

    let non_empty: Vec<&QuestionAnswer> = answers
        .iter()
        .filter(|a| !a.value.trim().is_empty())
        .collect();

    if !non_empty.is_empty() {
        out.push_str("User-provided context:\n");
        for a in non_empty {
            out.push_str(&format!(
                "- {}: {}\n",
                a.impact_dimension.as_str(),
                a.value.trim()
            ));
        }
    }
    out.push_str("[/CONTEXT]\n\n");
    out.push_str("Enhance the above input into a high-quality, precise prompt for an LLM.");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assemble_context_includes_original_and_dimensions() {
        let answers = vec![
            QuestionAnswer {
                question_id: "q1".into(),
                impact_dimension: ImpactDimension::Audience,
                value: "Manager".into(),
            },
            QuestionAnswer {
                question_id: "q2".into(),
                impact_dimension: ImpactDimension::Tone,
                value: "Formal".into(),
            },
        ];
        let ctx = assemble_context("write a leave mail", &answers);
        assert!(ctx.contains("[CONTEXT]"));
        assert!(ctx.contains("Original input: write a leave mail"));
        assert!(ctx.contains("- audience: Manager"));
        assert!(ctx.contains("- tone: Formal"));
        assert!(ctx.contains("[/CONTEXT]"));
        assert!(ctx.contains("Enhance the above input"));
    }

    #[test]
    fn assemble_context_omits_empty_answers() {
        let answers = vec![
            QuestionAnswer {
                question_id: "q1".into(),
                impact_dimension: ImpactDimension::Audience,
                value: "Client".into(),
            },
            QuestionAnswer {
                question_id: "q2".into(),
                impact_dimension: ImpactDimension::Tone,
                value: "   ".into(),
            },
        ];
        let ctx = assemble_context("draft something", &answers);
        assert!(ctx.contains("- audience: Client"));
        assert!(!ctx.contains("tone:"));
    }

    #[test]
    fn assemble_context_drops_user_context_section_when_no_answers() {
        let ctx = assemble_context("hello", &[]);
        assert!(ctx.contains("Original input: hello"));
        assert!(!ctx.contains("User-provided context:"));
    }
}
