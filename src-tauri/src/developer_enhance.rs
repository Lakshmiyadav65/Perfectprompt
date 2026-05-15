//! Developer-mode helper. Migration Step 9: this module used to build a
//! full `[CONTEXT]` envelope and call the LLM directly. Now it just
//! gathers the user's active project metadata into a
//! [`pipeline::DeveloperContext`] which the orchestrator merges into
//! the `<context>` section of the LLM user message.
//!
//! The function returns `None` to mean "no developer context" — either
//! the user has the developer-mode project-awareness setting off, or
//! they have no active project. The orchestrator handles both cases
//! uniformly: it just doesn't append a `<context>` block.

use std::path::Path;

use tauri::{AppHandle, Runtime};

use crate::active_app::ActiveAppContext;
use crate::pipeline::DeveloperContext;
use crate::{project_scan, projects, settings};

pub fn developer_context_for<R: Runtime>(
    app: &AppHandle<R>,
    _active_app: &ActiveAppContext,
) -> Option<DeveloperContext> {
    if !settings::load(app)
        .app_classification
        .use_project_awareness_in_developer_apps
    {
        return None;
    }
    let proj = projects::active_project_for(app)?;

    let mut summary = proj.description.trim().to_string();
    if !proj.links.is_empty() {
        if !summary.is_empty() {
            summary.push('\n');
        }
        summary.push_str(&format!("Links: {}", proj.links.join(", ")));
    }
    if let Some(path_str) = proj.path.as_deref().filter(|s| !s.trim().is_empty()) {
        if let Some(scan) = project_scan::scan_project_dir(Path::new(path_str)) {
            if !summary.is_empty() {
                summary.push('\n');
            }
            summary.push_str(&format!("Project scan:\n{scan}"));
        }
    }

    Some(DeveloperContext {
        project_name: proj.name,
        project_summary: summary,
    })
}
