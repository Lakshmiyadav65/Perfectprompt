use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, Runtime};

use crate::github_analyze;

const PROJECTS_FILE: &str = "projects.json";
const PROJECT_CACHE_DIR: &str = "project_cache";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub links: Vec<String>,
    /// Absolute path to the project directory. When set and the directory
    /// exists, the developer-mode enhancer scans it for README + manifest
    /// context. Optional so projects created before this field existed
    /// still deserialize cleanly.
    #[serde(default)]
    pub path: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProjectStore {
    pub active_project_id: Option<String>,
    pub projects: Vec<Project>,
}

fn store_path<R: Runtime>(app: &AppHandle<R>) -> Result<PathBuf> {
    let dir = app
        .path()
        .app_config_dir()
        .context("could not resolve app config dir")?;
    std::fs::create_dir_all(&dir).context("could not create app config dir")?;
    Ok(dir.join(PROJECTS_FILE))
}

/// Root directory for cached GitHub fetches. Lives at
/// `<app_config_dir>/project_cache/`. Does not create the directory —
/// [`github_analyze::fetch_and_cache`] does that lazily on first write.
pub fn cached_context_path<R: Runtime>(app: &AppHandle<R>) -> Result<PathBuf> {
    let dir = app
        .path()
        .app_config_dir()
        .context("could not resolve app config dir")?;
    Ok(dir.join(PROJECT_CACHE_DIR))
}

/// Return the first link in `links` that parses as a GitHub URL, or
/// `None` if no link does. Pure helper — used both by the background
/// fetch trigger and by the manual-refresh Tauri command.
fn pick_github_link(links: &[String]) -> Option<String> {
    links
        .iter()
        .find(|l| github_analyze::parse_github_url(l).is_some())
        .cloned()
}

/// Best-effort background GitHub fetch on project add/update. Fires
/// only when:
///   - the project has at least one github.com link, AND
///   - no cache file exists for this project yet.
///
/// The spawned task is detached — `add_project` / `update_project`
/// return to the caller immediately. A live fetch in flight when the
/// user triggers an enhancement does NOT block the enhancement: Stage D
/// reads whatever cached data exists at that moment.
fn maybe_spawn_github_fetch<R: Runtime>(
    app: &AppHandle<R>,
    project_id: &str,
    links: &[String],
) {
    let Some(url) = pick_github_link(links) else {
        return;
    };
    let cache_dir = match cached_context_path(app) {
        Ok(d) => d,
        Err(e) => {
            println!("[projects] cached_context_path failed: {e:#}");
            return;
        }
    };
    if github_analyze::cached_repo(&cache_dir, project_id).is_some() {
        return;
    }
    let project_id_owned = project_id.to_string();
    tauri::async_runtime::spawn(async move {
        match github_analyze::fetch_and_cache(&cache_dir, &project_id_owned, &url).await {
            Ok(_) => println!(
                "[projects] background github fetch cached for {project_id_owned}"
            ),
            Err(e) => println!(
                "[projects] background github fetch failed for {project_id_owned}: {e:#}"
            ),
        }
    });
}

pub fn load_store<R: Runtime>(app: &AppHandle<R>) -> ProjectStore {
    let Ok(path) = store_path(app) else {
        return ProjectStore::default();
    };
    match std::fs::read_to_string(&path) {
        Ok(s) => serde_json::from_str::<ProjectStore>(&s).unwrap_or_default(),
        Err(_) => ProjectStore::default(),
    }
}

fn save_store<R: Runtime>(app: &AppHandle<R>, store: &ProjectStore) -> Result<()> {
    let path = store_path(app)?;
    let json = serde_json::to_string_pretty(store).context("serialize projects")?;
    std::fs::write(&path, json).context("write projects.json")?;
    Ok(())
}

fn now_iso() -> String {
    // Simple ISO-ish timestamp without external crate
    let d = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    format!("{}s", d.as_secs())
}

fn gen_id() -> String {
    use std::time::SystemTime;
    let ts = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    format!("proj_{ts}")
}

// ---------- Tauri commands ----------

#[tauri::command]
pub fn list_projects<R: Runtime>(app: AppHandle<R>) -> ProjectStore {
    load_store(&app)
}

#[tauri::command]
pub fn get_active_project<R: Runtime>(app: AppHandle<R>) -> Option<Project> {
    active_project_for(&app)
}

/// Non-command helper — callable from other modules with a borrowed AppHandle.
pub fn active_project_for<R: Runtime>(app: &AppHandle<R>) -> Option<Project> {
    let store = load_store(app);
    let active_id = store.active_project_id.as_deref()?;
    store.projects.into_iter().find(|p| p.id == active_id)
}

#[tauri::command]
pub fn add_project<R: Runtime>(
    app: AppHandle<R>,
    name: String,
    description: String,
    path: Option<String>,
) -> std::result::Result<Project, String> {
    let trimmed_name = name.trim();
    if trimmed_name.is_empty() {
        return Err("Project name cannot be empty".into());
    }

    let now = now_iso();
    let project = Project {
        id: gen_id(),
        name: trimmed_name.to_string(),
        description: description.trim().to_string(),
        links: vec![],
        path: path.and_then(|p| {
            let trimmed = p.trim();
            if trimmed.is_empty() { None } else { Some(trimmed.to_string()) }
        }),
        created_at: now.clone(),
        updated_at: now,
    };

    let mut store = load_store(&app);
    store.projects.push(project.clone());

    // Auto-activate if this is the first project
    if store.active_project_id.is_none() {
        store.active_project_id = Some(project.id.clone());
    }

    save_store(&app, &store).map_err(|e| format!("{e:#}"))?;
    println!("[projects] added project '{}' ({})", project.name, project.id);
    maybe_spawn_github_fetch(&app, &project.id, &project.links);
    Ok(project)
}

#[tauri::command]
pub fn update_project<R: Runtime>(
    app: AppHandle<R>,
    id: String,
    name: String,
    description: String,
    links: Vec<String>,
    path: Option<String>,
) -> std::result::Result<Project, String> {
    let trimmed_name = name.trim();
    if trimmed_name.is_empty() {
        return Err("Project name cannot be empty".into());
    }

    let mut store = load_store(&app);
    let project = store
        .projects
        .iter_mut()
        .find(|p| p.id == id)
        .ok_or_else(|| format!("Project {id} not found"))?;

    project.name = trimmed_name.to_string();
    project.description = description.trim().to_string();
    project.links = links;
    project.path = path.and_then(|p| {
        let trimmed = p.trim();
        if trimmed.is_empty() { None } else { Some(trimmed.to_string()) }
    });
    project.updated_at = now_iso();
    let updated = project.clone();

    save_store(&app, &store).map_err(|e| format!("{e:#}"))?;
    println!("[projects] updated project '{}' ({})", updated.name, updated.id);
    maybe_spawn_github_fetch(&app, &updated.id, &updated.links);
    Ok(updated)
}

#[tauri::command]
pub fn delete_project<R: Runtime>(
    app: AppHandle<R>,
    id: String,
) -> std::result::Result<(), String> {
    let mut store = load_store(&app);
    let before = store.projects.len();
    store.projects.retain(|p| p.id != id);

    if store.projects.len() == before {
        return Err(format!("Project {id} not found"));
    }

    // If the deleted project was active, clear or reassign
    if store.active_project_id.as_deref() == Some(&id) {
        store.active_project_id = store.projects.first().map(|p| p.id.clone());
    }

    save_store(&app, &store).map_err(|e| format!("{e:#}"))?;
    println!("[projects] deleted project {id}");
    Ok(())
}

#[tauri::command]
pub fn set_active_project<R: Runtime>(
    app: AppHandle<R>,
    id: String,
) -> std::result::Result<(), String> {
    let mut store = load_store(&app);

    // Verify the project exists
    if !store.projects.iter().any(|p| p.id == id) {
        return Err(format!("Project {id} not found"));
    }

    store.active_project_id = Some(id.clone());
    save_store(&app, &store).map_err(|e| format!("{e:#}"))?;
    println!("[projects] set active project to {id}");
    Ok(())
}

/// Clear the active project. Used by the capsule's project picker
/// when the user selects "— no project —" to opt out of project
/// context for the next enhancement.
#[tauri::command]
pub fn clear_active_project<R: Runtime>(
    app: AppHandle<R>,
) -> std::result::Result<(), String> {
    let mut store = load_store(&app);
    if store.active_project_id.is_none() {
        return Ok(());
    }
    store.active_project_id = None;
    save_store(&app, &store).map_err(|e| format!("{e:#}"))?;
    println!("[projects] cleared active project");
    Ok(())
}

/// Read the cached GitHub fetch's `fetched_at` timestamp for a given
/// project. Returns `None` when no cache exists yet (e.g. the project
/// has no github.com link, or the background fetch hasn't completed,
/// or the user has never refreshed). The frontend uses this to render
/// "Last fetched: …" alongside the manual-refresh button (Step 9).
#[tauri::command]
pub fn get_cached_context_timestamp<R: Runtime>(
    app: AppHandle<R>,
    id: String,
) -> std::result::Result<Option<String>, String> {
    let cache_dir = cached_context_path(&app).map_err(|e| format!("{e:#}"))?;
    Ok(github_analyze::cached_repo(&cache_dir, &id).map(|c| c.fetched_at))
}

/// Manual-refresh path for the project's cached GitHub context. Used by
/// the "Refresh project context" button in `ProjectManager.tsx`
/// (Step 9). Overwrites any existing cache file. Awaits the fetch so
/// the frontend can show success / failure + the new fetched_at.
#[tauri::command]
pub async fn refresh_project_context<R: Runtime>(
    app: AppHandle<R>,
    id: String,
) -> std::result::Result<String, String> {
    let store = load_store(&app);
    let project = store
        .projects
        .iter()
        .find(|p| p.id == id)
        .ok_or_else(|| format!("Project {id} not found"))?;
    let url = pick_github_link(&project.links)
        .ok_or_else(|| "no github.com link on this project".to_string())?;
    let cache_dir = cached_context_path(&app).map_err(|e| format!("{e:#}"))?;
    let cached = github_analyze::fetch_and_cache(&cache_dir, &id, &url)
        .await
        .map_err(|e| format!("{e:#}"))?;
    println!(
        "[projects] manual refresh cached github for {id} at {}",
        cached.fetched_at
    );
    Ok(cached.fetched_at)
}

#[tauri::command]
pub fn read_file_content(path: String) -> std::result::Result<String, String> {
    let file_path = std::path::Path::new(&path);

    if !file_path.exists() {
        return Err(format!("File not found: {path}"));
    }

    // Read file as UTF-8 text (works for .txt, .md, .json, .csv, .html, .xml, code files, etc.)
    match std::fs::read_to_string(file_path) {
        Ok(content) => {
            println!("[projects] read file: {} ({} chars)", path, content.len());
            Ok(content)
        }
        Err(e) => {
            // If not valid UTF-8, try reading as lossy
            match std::fs::read(file_path) {
                Ok(bytes) => {
                    let content = String::from_utf8_lossy(&bytes).to_string();
                    println!("[projects] read file (lossy): {} ({} chars)", path, content.len());
                    Ok(content)
                }
                Err(_) => Err(format!("Failed to read file: {e}")),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pick_github_link_finds_first_github_url() {
        let links = vec![
            "https://example.com/something".to_string(),
            "https://github.com/foo/bar".to_string(),
            "https://github.com/baz/qux".to_string(),
        ];
        assert_eq!(
            pick_github_link(&links).as_deref(),
            Some("https://github.com/foo/bar")
        );
    }

    #[test]
    fn pick_github_link_returns_none_when_no_github() {
        let links = vec![
            "https://example.com/x".to_string(),
            "https://gitlab.com/foo/bar".to_string(),
        ];
        assert!(pick_github_link(&links).is_none());
    }

    #[test]
    fn pick_github_link_returns_none_on_empty_list() {
        assert!(pick_github_link(&[]).is_none());
    }

    #[test]
    fn pick_github_link_accepts_ssh_form() {
        let links = vec!["git@github.com:owner/repo.git".to_string()];
        assert_eq!(
            pick_github_link(&links).as_deref(),
            Some("git@github.com:owner/repo.git")
        );
    }
}
