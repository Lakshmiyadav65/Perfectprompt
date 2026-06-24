use std::collections::HashMap;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, Runtime};

use crate::question_bank::{
    detect_domain, static_questions_for, GeneratedQuestion, ImpactDimension,
};
use crate::{clipboard, generation, pipeline, tray};

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
    /// Session-scoped memory keyed by `impact_dimension` (e.g. "tone",
    /// "audience"). Frontend pre-fills matching questions with these
    /// values and shows a "remembered" badge (PRD §5.1.6 / §7.3).
    #[serde(default)]
    pub remembered_values: HashMap<String, String>,
}

/// Reads the captured input from `AppState.pending_prompt`, then waits up
/// to PRD §6.5's 3s budget for the in-flight LLM question-generation call
/// to complete. If the LLM returns at least `MIN_VALID_QUESTIONS`, those
/// are shown. Otherwise — on timeout, channel drop, error, or too-few
/// valid questions — the card falls back to the domain-specific static
/// bank so the user always sees questions within the latency budget.
#[tauri::command]
pub async fn fetch_question_card_session(app: AppHandle) -> QuestionSession {
    let t_fetch = Instant::now();
    let state = app.state::<crate::AppState>();

    let original = {
        let pending = state.pending_prompt.lock().unwrap();
        pending.clone()
    };
    let domain = detect_domain(&original);

    let remembered_values = {
        let map = state.remembered_answers.lock().unwrap();
        map.clone()
    };

    let rx = {
        let mut guard = state.pending_questions.lock().unwrap();
        guard.take()
    };

    let (questions, source) = match rx {
        Some(rx) => {
            let timeout = Duration::from_secs(generation::GENERATION_TIMEOUT_SECS);
            match tokio::time::timeout(timeout, rx).await {
                Ok(Ok(Ok(qs))) if qs.len() >= generation::MIN_VALID_QUESTIONS => (qs, "llm"),
                Ok(Ok(Ok(qs))) => {
                    println!(
                        "[clarify] LLM returned only {} question(s) (< {}); using static bank",
                        qs.len(),
                        generation::MIN_VALID_QUESTIONS
                    );
                    (static_questions_for(domain), "static (llm-empty)")
                }
                Ok(Ok(Err(e))) => {
                    println!("[clarify] LLM generation failed ({e}); using static bank");
                    (static_questions_for(domain), "static (llm-error)")
                }
                Ok(Err(_canceled)) => {
                    println!("[clarify] LLM channel dropped; using static bank");
                    (static_questions_for(domain), "static (channel-dropped)")
                }
                Err(_elapsed) => {
                    println!(
                        "[clarify] LLM generation exceeded {}s budget; using static bank",
                        generation::GENERATION_TIMEOUT_SECS
                    );
                    (static_questions_for(domain), "static (timeout)")
                }
            }
        }
        None => (static_questions_for(domain), "static (no-generation)"),
    };

    println!(
        "[latency] fetch_question_card_session ready in {}ms (source={}, {} question(s), PRD §12 target <3s)",
        t_fetch.elapsed().as_millis(),
        source,
        questions.len()
    );

    QuestionSession {
        original_input: original,
        questions,
        answers: Vec::new(),
        remembered_values,
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
    let t_submit = Instant::now();
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

    // Step 9 rewire: the [CONTEXT] envelope is sent through the new
    // pipeline as raw_input. Stages A, C, and E still apply — the
    // envelope just looks like a longer input. `active_app` is set to
    // the sentinel "clarify" because by submit time the user is on the
    // card window, not their original target app.
    let pi = pipeline::PipelineInput {
        raw_input: combined_input,
        active_app: "clarify".to_string(),
    };
    let output = pipeline::run(&app, pi)
        .await
        .map_err(|e| format!("pipeline failed: {e:#}"))?;
    let enhanced = output.final_text.clone();

    // Record successful answers in session-scoped memory (PRD §5.1.6).
    // Empty answers are ignored — leaving a dimension blank should not
    // overwrite a previously remembered value.
    {
        let state = app.state::<crate::AppState>();
        let mut mem = state.remembered_answers.lock().unwrap();
        for a in &answers {
            let trimmed = a.value.trim();
            if !trimmed.is_empty() {
                mem.insert(a.impact_dimension.as_str().to_string(), trimmed.to_string());
            }
        }
    }

    if let Some(window) = app.get_webview_window("question-card") {
        let _ = window.hide();
        // Brief settle so the OS restores focus to the user's target app
        // before we synthesize Ctrl+V.
        tokio::time::sleep(Duration::from_millis(150)).await;
    }

    clipboard::replace_selection(&app, &enhanced)
        .await
        .map_err(|e| format!("paste failed: {e}"))?;

    if output.used_fallback {
        if let Some(reason) = &output.fallback_reason {
            tray::notify_fallback(&app, reason);
        }
    }

    println!(
        "[latency] submit→pasted={}ms route={} fallback={} (PRD §12 budget 1.5–3s)",
        t_submit.elapsed().as_millis(),
        output.trace.route,
        output.used_fallback,
    );
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
