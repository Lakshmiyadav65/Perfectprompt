use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, Runtime};

const PROJECTS_FILE: &str = "projects.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub links: Vec<String>,
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
    Ok(project)
}

#[tauri::command]
pub fn update_project<R: Runtime>(
    app: AppHandle<R>,
    id: String,
    name: String,
    description: String,
    links: Vec<String>,
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
    project.updated_at = now_iso();
    let updated = project.clone();

    save_store(&app, &store).map_err(|e| format!("{e:#}"))?;
    println!("[projects] updated project '{}' ({})", updated.name, updated.id);
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
