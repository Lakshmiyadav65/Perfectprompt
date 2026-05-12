use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, Runtime};

use crate::question_bank::{GeneratedQuestion, ImpactDimension, QuestionType};

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

/// Returns a placeholder session so the QuestionCard shell renders during
/// scaffolding. Task 2 replaces this with `question_bank::detect_domain` +
/// `question_bank::static_questions_for`. Task 3 layers LLM generation on top.
#[tauri::command]
pub fn fetch_question_card_session<R: Runtime>(_app: AppHandle<R>) -> QuestionSession {
    let state = _app.state::<crate::AppState>();
    let original = state.pending_prompt.lock().unwrap().clone();

    QuestionSession {
        original_input: if original.is_empty() {
            "(scaffolding) no captured input yet".into()
        } else {
            original
        },
        questions: scaffolding_questions(),
        answers: Vec::new(),
    }
}

/// Receives the user's answers. For scaffolding this just logs the payload —
/// Task 2 wires it into `enhance::assemble_context` + the existing
/// enhancement pipeline.
#[tauri::command]
pub fn submit_question_card_answers<R: Runtime>(
    _app: AppHandle<R>,
    answers: Vec<QuestionAnswer>,
) -> Result<(), String> {
    println!(
        "[clarify] received {} answer(s) (scaffolding stub)",
        answers.len()
    );
    for answer in &answers {
        println!(
            "[clarify]   {:?} ({}): {}",
            answer.impact_dimension, answer.question_id, answer.value
        );
    }
    Ok(())
}

/// Opens the question-card window. Wired to the tray menu during scaffolding;
/// Task 3 will invoke this from the hotkey pipeline after complexity scoring.
#[tauri::command]
pub fn open_question_card<R: Runtime>(app: AppHandle<R>) -> Result<(), String> {
    let window = app
        .get_webview_window("question-card")
        .ok_or_else(|| "question-card window not found".to_string())?;
    window.show().map_err(|e| format!("show: {e}"))?;
    window.set_focus().map_err(|e| format!("focus: {e}"))?;
    Ok(())
}

/// Cancels the card and hides the window. The PRD requires that closing the
/// card preserves the original clipboard — we just hide for now since the
/// scaffolding doesn't touch the clipboard yet.
#[tauri::command]
pub fn cancel_question_card<R: Runtime>(app: AppHandle<R>) -> Result<(), String> {
    if let Some(window) = app.get_webview_window("question-card") {
        window.hide().map_err(|e| format!("hide: {e}"))?;
    }
    Ok(())
}

fn scaffolding_questions() -> Vec<GeneratedQuestion> {
    vec![
        GeneratedQuestion {
            id: "q1".into(),
            question: "Who is this for?".into(),
            question_type: QuestionType::Chips,
            options: vec!["Manager".into(), "Client".into(), "Team".into()],
            placeholder: None,
            impact_dimension: ImpactDimension::Audience,
            required: false,
        },
        GeneratedQuestion {
            id: "q2".into(),
            question: "What tone?".into(),
            question_type: QuestionType::Chips,
            options: vec!["Formal".into(), "Neutral".into(), "Casual".into()],
            placeholder: None,
            impact_dimension: ImpactDimension::Tone,
            required: false,
        },
        GeneratedQuestion {
            id: "q3".into(),
            question: "Any extra context?".into(),
            question_type: QuestionType::FreeText,
            options: Vec::new(),
            placeholder: Some("e.g. follow-up to last week's call".into()),
            impact_dimension: ImpactDimension::Other,
            required: false,
        },
    ]
}
