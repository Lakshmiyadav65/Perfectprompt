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
    /// Project Knowledge — full directory digest (Repomix-style packed
    /// README + manifests + key source files). Populated by the
    /// `digest_local_project` / `digest_github_project` commands;
    /// cleared by `clear_project_digest`. Optional + `#[serde(default)]`
    /// so projects created before this field existed still load cleanly.
    /// Used by the digester for `View digest` inspection and as input
    /// to PROJECT.md regeneration; **no longer injected verbatim** into
    /// per-enhance context blocks (see `project_summary` below).
    #[serde(default)]
    pub digest: Option<crate::repo_digest::RepoDigest>,
    /// Project Knowledge rethink (architecture v2) — curated ~2 KB
    /// PROJECT.md summary, auto-generated once from the digest then
    /// editable by the user. This is what now gets injected into the
    /// LLM's `<context>` block on every enhance/polish call (along with
    /// the digest's directory_structure as `<file_index>`). The shift
    /// from "inject full 120 KB digest" to "inject curated 2 KB
    /// summary" is what fixes the reading-comprehension failures.
    /// Optional + `#[serde(default)]` for backwards compatibility with
    /// projects.json files predating this field.
    #[serde(default)]
    pub project_summary: Option<crate::project_summary::ProjectSummary>,
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
        digest: None,
        project_summary: None,
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

// ─────────────────────────────────────────────────────────────────────
// Project Knowledge — digest commands
// ─────────────────────────────────────────────────────────────────────
//
// `digest_local_project` and `digest_github_project` are the two
// user-facing entry points for the Project Knowledge feature. Both
// converge on `repo_digest::digest_directory` after producing a
// directory path on disk (the local picker hands one in; the GitHub
// flow downloads + extracts a tarball first).
//
// `clear_project_digest` nulls the digest field. The cached extracted
// tarball under `project_cache/repos/{project_id}/` is intentionally
// left in place — it's regenerated on the next refresh, and keeping
// it around means a subsequent "Re-add" picks up instantly if the
// user changes their mind.

/// Path to the per-project tarball extraction directory.
fn project_tarball_dir<R: Runtime>(
    app: &AppHandle<R>,
    project_id: &str,
) -> Result<PathBuf> {
    let base = cached_context_path(app)?;
    Ok(base.join("repos").join(project_id))
}

/// Update the named project's `digest` field and persist projects.json.
/// Pulled out into a helper so both digest commands share the exact
/// same persistence path.
fn persist_digest<R: Runtime>(
    app: &AppHandle<R>,
    project_id: &str,
    digest: crate::repo_digest::RepoDigest,
) -> Result<()> {
    let mut store = load_store(app);
    let project = store
        .projects
        .iter_mut()
        .find(|p| p.id == project_id)
        .ok_or_else(|| anyhow::anyhow!("Project {project_id} not found"))?;
    project.digest = Some(digest);
    project.updated_at = now_iso();
    save_store(app, &store)?;
    Ok(())
}

/// Digest a local folder the user picked via the OS folder dialog.
/// Runs the actual walk on a blocking thread because traversing
/// thousands of small files would otherwise pinch the async runtime.
#[tauri::command]
pub async fn digest_local_project<R: Runtime>(
    app: AppHandle<R>,
    project_id: String,
    folder_path: String,
) -> std::result::Result<crate::repo_digest::RepoDigest, String> {
    let path = PathBuf::from(folder_path.trim());
    if path.as_os_str().is_empty() {
        return Err("Folder path is empty".into());
    }
    if !path.is_absolute() {
        return Err("Folder path must be absolute".into());
    }
    if !path.exists() {
        return Err(format!("Path does not exist: {}", path.display()));
    }
    if !path.is_dir() {
        return Err(format!("Path is not a directory: {}", path.display()));
    }

    let source = crate::repo_digest::DigestSource::Local {
        path: path.to_string_lossy().to_string(),
    };
    let path_for_task = path.clone();
    let digest = tokio::task::spawn_blocking(move || {
        crate::repo_digest::digest_directory(
            &path_for_task,
            source,
            &crate::repo_digest::DigestConfig::default(),
        )
    })
    .await
    .map_err(|e| format!("digest task failed: {e}"))?
    .map_err(|e| format!("{e:#}"))?;

    persist_digest(&app, &project_id, digest.clone()).map_err(|e| format!("{e:#}"))?;
    println!(
        "[projects] local digest stored for {project_id}: {} files, {} elided, {} tokens",
        digest.file_count, digest.elided_count, digest.token_count_estimate
    );

    // Project Knowledge rethink — one-shot summarisation step.
    // Best-effort: if the LLM call or schema validation fails the
    // project still works via the legacy full-digest fallback in
    // build_context_block. The user can manually retry from the UI
    // via Regenerate.
    try_generate_and_store_summary(&app, &project_id, &digest).await;

    Ok(digest)
}

/// Digest a public GitHub repo: parse the URL → resolve the default
/// branch (cached metadata first, fresh analyze second) → download
/// the codeload tarball → extract → run the same digester as the
/// local path on the extracted dir.
///
/// Every fallible step emits an `[digest-github] step=N ...` line at
/// its start AND tags any error with its step name before returning
/// it verbatim to the frontend. The previous symptom — UI "appears
/// to do nothing" — was masked by step-untagged errors that the
/// frontend then dropped on the floor. Now any failure surfaces both
/// in the dev console (tail) and in the project edit form's
/// `digestError` banner.
///
/// As of v1.5 the hosted edge function accepts a `context_block`
/// field, so the digest reaches the LLM on both BYOK and hosted
/// paths (see `pipeline.rs::build_context_block` for the
/// pipeline-side wiring).
#[tauri::command]
pub async fn digest_github_project<R: Runtime>(
    app: AppHandle<R>,
    project_id: String,
    github_url: String,
) -> std::result::Result<crate::repo_digest::RepoDigest, String> {
    eprintln!(
        "[digest-github] step=0 project_id={} url={}",
        project_id, github_url
    );
    let url = github_url.trim().to_string();
    if url.is_empty() {
        return Err("step=0 parse: GitHub URL is empty".into());
    }

    // ── step 1 — parse URL ──────────────────────────────────────────
    eprintln!("[digest-github] step=1 parse_url url={url}");
    let (owner, repo) = github_analyze::parse_github_url(&url).ok_or_else(|| {
        format!("step=1 parse_url: not a recognisable GitHub URL: {url}")
    })?;
    eprintln!("[digest-github] step=1 parse_url ok owner={owner} repo={repo}");

    // ── step 2 — resolve default branch ─────────────────────────────
    // Strict precedence: cached metadata from a prior analyze_github_repo
    // call first (zero network), live analyze_github_repo second
    // (~1 round-trip to GitHub's API for the default_branch field),
    // and ONLY as an absolute fallback default to "master" — which
    // is what the real-world test repos (expressjs/express,
    // tj/commander.js, octocat/Hello-World) actually use. Never
    // hardcode "main".
    eprintln!("[digest-github] step=2 resolve_branch");
    let cache_dir = cached_context_path(&app)
        .map_err(|e| format!("step=2 resolve_branch (cache_dir): {e:#}"))?;
    let cached_branch = github_analyze::cached_repo(&cache_dir, &project_id)
        .map(|c| c.repo.default_branch);
    let (branch, html_url) = if let Some(b) = cached_branch.clone().filter(|s| !s.is_empty()) {
        eprintln!("[digest-github] step=2 resolve_branch hit_cache branch={b}");
        // We still need html_url for DigestSource. Pull from cache too.
        let cached_html_url = github_analyze::cached_repo(&cache_dir, &project_id)
            .map(|c| c.repo.html_url)
            .unwrap_or_else(|| url.clone());
        (b, cached_html_url)
    } else {
        eprintln!("[digest-github] step=2 resolve_branch miss_cache, calling analyze_github_repo");
        match github_analyze::analyze_github_repo(url.clone()).await {
            Ok(a) => {
                let b = if a.default_branch.is_empty() {
                    "master".to_string()
                } else {
                    a.default_branch.clone()
                };
                eprintln!("[digest-github] step=2 resolve_branch ok branch={b}");
                (b, a.html_url)
            }
            Err(e) => {
                // GitHub's API might be rate-limited or unreachable.
                // If we have ANY cached branch (even if filtered as
                // empty earlier), use it. Otherwise fall back to
                // "master" — the dominant default among the projects
                // we care about — rather than failing outright.
                let fallback_branch = cached_branch
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| "master".to_string());
                eprintln!(
                    "[digest-github] step=2 resolve_branch analyze_failed={e}, falling back to branch={fallback_branch}"
                );
                (fallback_branch, url.clone())
            }
        }
    };

    // ── step 3 — locate tarball cache dir ───────────────────────────
    eprintln!("[digest-github] step=3 tarball_cache_dir project_id={project_id}");
    let tarball_dir = project_tarball_dir(&app, &project_id)
        .map_err(|e| format!("step=3 tarball_cache_dir: {e:#}"))?;
    eprintln!(
        "[digest-github] step=3 tarball_cache_dir ok path={}",
        tarball_dir.display()
    );

    // ── step 4 — download + extract tarball ─────────────────────────
    eprintln!(
        "[digest-github] step=4 fetch_tarball owner={owner} repo={repo} branch={branch}"
    );
    let extracted = crate::repo_digest::fetch_github_tarball(
        &owner, &repo, &branch, &tarball_dir,
    )
    .await
    .map_err(|e| {
        format!("step=4 fetch_tarball ({owner}/{repo}@{branch}): {e:#}")
    })?;
    eprintln!(
        "[digest-github] step=4 fetch_tarball ok extracted={}",
        extracted.display()
    );

    // ── step 5 — run the digester on the extracted dir ──────────────
    eprintln!("[digest-github] step=5 digest_directory");
    let source = crate::repo_digest::DigestSource::Github {
        owner: owner.clone(),
        repo: repo.clone(),
        branch: branch.clone(),
        html_url,
    };
    let extracted_for_task = extracted.clone();
    let digest = tokio::task::spawn_blocking(move || {
        crate::repo_digest::digest_directory(
            &extracted_for_task,
            source,
            &crate::repo_digest::DigestConfig::default(),
        )
    })
    .await
    .map_err(|e| format!("step=5 digest_directory (task join): {e}"))?
    .map_err(|e| format!("step=5 digest_directory: {e:#}"))?;
    eprintln!(
        "[digest-github] step=5 digest_directory ok files={} elided={} tokens={} bytes={}",
        digest.file_count,
        digest.elided_count,
        digest.token_count_estimate,
        digest.digest_text.len()
    );

    // ── step 6 — persist digest onto the Project + save store ───────
    eprintln!("[digest-github] step=6 persist_digest project_id={project_id}");
    persist_digest(&app, &project_id, digest.clone())
        .map_err(|e| format!("step=6 persist_digest: {e:#}"))?;
    eprintln!("[digest-github] step=6 persist_digest ok");
    println!(
        "[projects] github digest stored for {project_id} ({}/{}@{}): {} files, {} elided",
        owner, repo, branch, digest.file_count, digest.elided_count
    );

    // ── step 7 — generate PROJECT.md from the digest ────────────────
    // One-shot LLM call. Best-effort: if it fails the digest is still
    // saved, and per-enhance calls fall back to legacy full-digest
    // injection until the user clicks Regenerate.
    eprintln!("[digest-github] step=7 project_summary_generate");
    try_generate_and_store_summary(&app, &project_id, &digest).await;

    Ok(digest)
}

// ─────────────────────────────────────────────────────────────────────
// Project Knowledge rethink — PROJECT.md generation + edit commands
// ─────────────────────────────────────────────────────────────────────
//
// `try_generate_and_store_summary` is the post-digest hook. Both
// `digest_local_project` and `digest_github_project` call it as their
// final step. It is intentionally best-effort: a failed summary
// generation does NOT fail the digest command (the digest is already
// saved, and pipeline.rs::build_context_block has a fallback path
// that injects the full digest_text when project_summary is None).
//
// The user-facing controls are:
//   - `update_project_summary`: save manual edits from the editor
//     modal. Sets user_edited=true.
//   - `regenerate_project_summary`: re-run the LLM generator against
//     the current digest. Resets user_edited=false. The frontend
//     surfaces a confirm dialog before calling this when
//     user_edited is true.

async fn try_generate_and_store_summary<R: Runtime>(
    app: &AppHandle<R>,
    project_id: &str,
    digest: &crate::repo_digest::RepoDigest,
) {
    match crate::project_summary::generate_project_summary(app, digest).await {
        Ok(md) => {
            let summary = crate::project_summary::ProjectSummary {
                markdown: md,
                generated_at: now_iso_8601(),
                user_edited: false,
                generator_model: "llama-3.3-70b-versatile".to_string(),
            };
            if let Err(e) = persist_summary(app, project_id, summary) {
                eprintln!(
                    "[project-summary] persist failed for {project_id}: {e:#} — \
                     digest still saved, pipeline will use legacy fallback"
                );
            } else {
                println!(
                    "[project-summary] generated and stored for {project_id}"
                );
            }
        }
        Err(e) => {
            // Fallback path will kick in on next enhance — don't
            // surface this as a hard error to the user, just log so
            // a tail of the dev console shows what happened.
            eprintln!(
                "[project-summary] generation failed for {project_id}: {e:#} — \
                 pipeline will use legacy fallback"
            );
        }
    }
}

fn persist_summary<R: Runtime>(
    app: &AppHandle<R>,
    project_id: &str,
    summary: crate::project_summary::ProjectSummary,
) -> Result<()> {
    let mut store = load_store(app);
    let project = store
        .projects
        .iter_mut()
        .find(|p| p.id == project_id)
        .ok_or_else(|| anyhow::anyhow!("Project {project_id} not found"))?;
    project.project_summary = Some(summary);
    project.updated_at = now_iso_8601();
    save_store(app, &store)?;
    Ok(())
}

/// ISO-8601 (UTC, Z suffix). Used for ProjectSummary.generated_at —
/// distinct from `now_iso()` above which returns the legacy
/// `"{secs}s"` shape preserved for backwards compatibility with the
/// existing `created_at` / `updated_at` consumers in the UI.
fn now_iso_8601() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = (secs / 86_400) as i64;
    let secs_of_day = secs % 86_400;
    let hh = (secs_of_day / 3600) as u32;
    let mm = ((secs_of_day % 3600) / 60) as u32;
    let ss = (secs_of_day % 60) as u32;
    let (y, mo, d) = civil_from_days_iso(days);
    format!("{y:04}-{mo:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}Z")
}

fn civil_from_days_iso(days: i64) -> (i32, u32, u32) {
    // Howard Hinnant's days-from-civil; same algorithm as the
    // implementation in trace.rs / enhancement_history.rs (kept local
    // here to avoid widening another module's API for a one-line
    // helper).
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { (mp + 3) as u32 } else { (mp - 9) as u32 };
    let y = if m <= 2 { y + 1 } else { y };
    (y as i32, m, d)
}

/// User-edit save path. The frontend ProjectSummaryEditor calls
/// this with the rejoined markdown after the user clicks Save.
/// Schema is validated server-side so the editor can't persist a
/// malformed summary (which would degrade subsequent enhance calls).
#[tauri::command]
pub fn update_project_summary<R: Runtime>(
    app: AppHandle<R>,
    project_id: String,
    markdown: String,
) -> std::result::Result<(), String> {
    let validated = crate::project_summary::validate_project_md(&markdown)
        .map_err(|e| format!("{e:#}"))?;
    let summary = crate::project_summary::ProjectSummary {
        markdown: validated,
        generated_at: now_iso_8601(),
        user_edited: true,
        generator_model: "user-edit".to_string(),
    };
    persist_summary(&app, &project_id, summary).map_err(|e| format!("{e:#}"))?;
    println!("[project-summary] user edit saved for {project_id}");
    Ok(())
}

/// Manual regeneration path. Re-runs the LLM generator against the
/// current digest and overwrites whatever was previously stored
/// (including any user edits — the frontend warns first when
/// user_edited is true). Errors out if the project has no digest
/// (i.e. user clicked Regenerate before adding source content).
#[tauri::command]
pub async fn regenerate_project_summary<R: Runtime>(
    app: AppHandle<R>,
    project_id: String,
) -> std::result::Result<crate::project_summary::ProjectSummary, String> {
    let store = load_store(&app);
    let project = store
        .projects
        .iter()
        .find(|p| p.id == project_id)
        .ok_or_else(|| format!("Project {project_id} not found"))?;
    let digest = project.digest.as_ref().ok_or_else(|| {
        "Project has no digest — add a local folder or GitHub repo first.".to_string()
    })?;
    let md = crate::project_summary::generate_project_summary(&app, digest)
        .await
        .map_err(|e| format!("{e:#}"))?;
    let summary = crate::project_summary::ProjectSummary {
        markdown: md,
        generated_at: now_iso_8601(),
        user_edited: false,
        generator_model: "llama-3.3-70b-versatile".to_string(),
    };
    persist_summary(&app, &project_id, summary.clone())
        .map_err(|e| format!("{e:#}"))?;
    println!("[project-summary] regenerated for {project_id}");
    Ok(summary)
}

/// Wipe the digest off a project. Does NOT delete the cached
/// extracted tarball — that lives until the next refresh overwrites
/// it. Just nulls the field so the pipeline stops injecting the digest
/// into LLM calls.
#[tauri::command]
pub fn clear_project_digest<R: Runtime>(
    app: AppHandle<R>,
    project_id: String,
) -> std::result::Result<(), String> {
    let mut store = load_store(&app);
    let project = store
        .projects
        .iter_mut()
        .find(|p| p.id == project_id)
        .ok_or_else(|| format!("Project {project_id} not found"))?;
    project.digest = None;
    project.updated_at = now_iso();
    save_store(&app, &store).map_err(|e| format!("{e:#}"))?;
    println!("[projects] cleared digest for {project_id}");
    Ok(())
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
