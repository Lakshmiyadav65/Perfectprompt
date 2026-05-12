use anyhow::Result;
use tauri::{AppHandle, Runtime};

use crate::active_app::ActiveAppContext;
use crate::{enhance, projects, settings};

/// Direct enhancement path used when the active app is classified as a
/// developer environment. Skips the questionnaire and feeds the LLM the
/// captured prompt plus (optionally) the active project's awareness
/// context, framed inside the same `[CONTEXT]` envelope the
/// questionnaire path uses so the meta-prompt sees a uniform shape.
pub async fn enhance_for_developer<R: Runtime>(
    app: &AppHandle<R>,
    input: &str,
    active_app: &ActiveAppContext,
) -> Result<String> {
    let user_settings = settings::load(app);
    let use_project = user_settings
        .app_classification
        .use_project_awareness_in_developer_apps;

    let active_project = if use_project {
        projects::active_project_for(app)
    } else {
        None
    };

    let combined = build_developer_context(input, active_app, active_project.as_ref());
    println!(
        "[developer_enhance] context built ({} chars, project={})",
        combined.len(),
        active_project.as_ref().map(|p| p.name.as_str()).unwrap_or("none"),
    );

    enhance::enhance_prompt(app, &combined).await
}

fn build_developer_context(
    input: &str,
    active_app: &ActiveAppContext,
    project: Option<&projects::Project>,
) -> String {
    let mut out = String::new();
    out.push_str("[CONTEXT]\n");
    out.push_str(&format!("Original input: {}\n", input.trim()));
    out.push_str("Mode: developer (skip-questionnaire)\n");

    if !active_app.is_empty() {
        out.push_str("Active developer surface:\n");
        if !active_app.process_name.is_empty() {
            out.push_str(&format!("- process: {}\n", active_app.process_name));
        }
        if !active_app.window_title.is_empty() {
            out.push_str(&format!("- window: {}\n", active_app.window_title));
        }
    }

    if let Some(proj) = project {
        out.push_str(&format!("Active project: {}\n", proj.name));
        let desc = proj.description.trim();
        if !desc.is_empty() {
            out.push_str("Project context:\n");
            out.push_str(desc);
            out.push('\n');
        }
        if !proj.links.is_empty() {
            out.push_str("Project links:\n");
            for link in &proj.links {
                out.push_str(&format!("- {link}\n"));
            }
        }
    }

    out.push_str("[/CONTEXT]\n\n");
    out.push_str(
        "Rewrite the input above into a precise, actionable developer prompt for a coding \
agent. Preserve the user's original intent. Use the active project context to fill in \
missing technical details (files, frameworks, APIs, constraints, acceptance criteria) \
when relevant. Do not invent project facts that aren't supported by the context. Avoid \
marketing tone. Output only the final enhanced prompt.",
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_app() -> ActiveAppContext {
        ActiveAppContext {
            process_name: "Code.exe".into(),
            executable_path: "C:/.../Code.exe".into(),
            window_title: "main.rs - promptforge - Visual Studio Code".into(),
            pid: 42,
        }
    }

    fn fake_project() -> projects::Project {
        projects::Project {
            id: "proj_1".into(),
            name: "PromptForge".into(),
            description: "Tauri 2 + React app for system-tray prompt enhancement.".into(),
            links: vec!["https://github.com/example/promptforge".into()],
            created_at: "0".into(),
            updated_at: "0".into(),
        }
    }

    #[test]
    fn includes_active_app_metadata() {
        let ctx = build_developer_context("fix the off-by-one", &fake_app(), None);
        assert!(ctx.contains("Original input: fix the off-by-one"));
        assert!(ctx.contains("process: Code.exe"));
        assert!(ctx.contains("window: main.rs - promptforge"));
        assert!(ctx.contains("[CONTEXT]") && ctx.contains("[/CONTEXT]"));
    }

    #[test]
    fn includes_project_context_when_present() {
        let proj = fake_project();
        let ctx = build_developer_context("refactor the hotkey module", &fake_app(), Some(&proj));
        assert!(ctx.contains("Active project: PromptForge"));
        assert!(ctx.contains("Tauri 2 + React app"));
        assert!(ctx.contains("https://github.com/example/promptforge"));
    }

    #[test]
    fn omits_project_section_when_none() {
        let ctx = build_developer_context("write a unit test", &fake_app(), None);
        assert!(!ctx.contains("Active project"));
        assert!(!ctx.contains("Project context"));
    }

    #[test]
    fn omits_active_app_section_when_empty() {
        let ctx = build_developer_context("ship it", &ActiveAppContext::default(), None);
        assert!(!ctx.contains("Active developer surface"));
        assert!(ctx.contains("Original input: ship it"));
    }
}
