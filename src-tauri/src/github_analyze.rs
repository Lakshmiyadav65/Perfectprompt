use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};

const API_BASE: &str = "https://api.github.com";
const RAW_BASE: &str = "https://raw.githubusercontent.com";
const USER_AGENT: &str = "perfectprompt-app";
const TIMEOUT_SECS: u64 = 8;

/// Maximum README chars stitched into the description. Keeps the form
/// from showing a wall of text when the user opens the modal.
const MAX_README_CHARS: usize = 1500;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalyzedRepo {
    pub name: String,
    pub description: String,
    pub default_branch: String,
    pub html_url: String,
}

/// On-disk cache envelope. Wraps the fetched [`AnalyzedRepo`] with the
/// timestamp at which it was fetched so the UI can show "last fetched
/// 2 days ago" without needing the file's mtime.
///
/// Persisted at `<cache_dir>/{project_id}.json`. The cache directory is
/// supplied by callers (typically `<app_config_dir>/project_cache/`) so
/// these helpers stay AppHandle-free and unit-testable.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedRepo {
    pub repo: AnalyzedRepo,
    pub fetched_at: String,
}

#[derive(Debug, Deserialize)]
struct GithubRepoMeta {
    name: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    default_branch: Option<String>,
    html_url: String,
}

/// Tauri command entry point. Takes a user-pasted URL, returns the
/// analyzed payload that the React form auto-fills.
#[tauri::command]
pub async fn analyze_github_repo(url: String) -> std::result::Result<AnalyzedRepo, String> {
    analyze(&url).await.map_err(|e| format!("{e:#}"))
}

async fn analyze(url: &str) -> Result<AnalyzedRepo> {
    let (owner, repo) = parse_github_url(url)
        .ok_or_else(|| anyhow!("not a recognisable GitHub URL: {url}"))?;

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(TIMEOUT_SECS))
        .user_agent(USER_AGENT)
        .build()
        .context("could not build HTTP client")?;

    let meta_url = format!("{API_BASE}/repos/{owner}/{repo}");
    let resp = client
        .get(&meta_url)
        .header("accept", "application/vnd.github+json")
        .send()
        .await
        .with_context(|| format!("GitHub API request failed ({meta_url})"))?;

    let status = resp.status();
    if status == reqwest::StatusCode::NOT_FOUND {
        return Err(anyhow!("repo not found or private: {owner}/{repo}"));
    }
    if status == reqwest::StatusCode::FORBIDDEN {
        return Err(anyhow!(
            "GitHub API rate-limited (60 req/hour for unauthenticated). Try again later."
        ));
    }
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(anyhow!("GitHub API returned {status}: {body}"));
    }

    let meta: GithubRepoMeta = resp.json().await.context("invalid JSON from GitHub API")?;
    let default_branch = meta.default_branch.unwrap_or_else(|| "main".to_string());
    let upstream_description = meta.description.unwrap_or_default();

    let readme = fetch_readme(&client, &owner, &repo, &default_branch).await;
    let description = build_description(&upstream_description, readme.as_deref());

    Ok(AnalyzedRepo {
        name: meta.name,
        description,
        default_branch,
        html_url: meta.html_url,
    })
}

async fn fetch_readme(
    client: &reqwest::Client,
    owner: &str,
    repo: &str,
    branch: &str,
) -> Option<String> {
    // Try the common filename variants in decreasing order of likelihood.
    // Stops on the first success.
    const CANDIDATES: &[&str] = &["README.md", "readme.md", "README.rst", "README.txt", "README"];

    for name in CANDIDATES {
        let url = format!("{RAW_BASE}/{owner}/{repo}/{branch}/{name}");
        let resp = match client.get(&url).send().await {
            Ok(r) => r,
            Err(_) => continue,
        };
        if resp.status().is_success() {
            if let Ok(text) = resp.text().await {
                return Some(truncate_chars(&text, MAX_README_CHARS));
            }
        }
    }
    None
}

fn build_description(upstream: &str, readme: Option<&str>) -> String {
    let upstream = upstream.trim();
    let readme = readme.unwrap_or("").trim();

    if upstream.is_empty() && readme.is_empty() {
        return String::new();
    }
    if readme.is_empty() {
        return upstream.to_string();
    }
    if upstream.is_empty() {
        return format!("--- README ---\n{readme}");
    }
    format!("{upstream}\n\n--- README ---\n{readme}")
}

/// Pulls owner/repo out of the various GitHub URL shapes a user might
/// paste. Returns None for anything we can't recognise.
pub fn parse_github_url(input: &str) -> Option<(String, String)> {
    let s = input.trim();
    let s = s.strip_suffix('/').unwrap_or(s);
    // Strip the scheme so we can match raw `github.com/...` paste too.
    let stripped = s
        .strip_prefix("https://")
        .or_else(|| s.strip_prefix("http://"))
        .unwrap_or(s);
    let stripped = stripped
        .strip_prefix("www.")
        .unwrap_or(stripped);
    // SSH-style: git@github.com:owner/repo(.git)
    if let Some(rest) = stripped.strip_prefix("git@github.com:") {
        return owner_repo_from_path(rest);
    }
    let rest = stripped.strip_prefix("github.com/")?;
    owner_repo_from_path(rest)
}

fn owner_repo_from_path(path: &str) -> Option<(String, String)> {
    // Discard anything after owner/repo (e.g. /tree/main, /pull/123).
    let mut parts = path.split('/');
    let owner = parts.next()?.trim();
    let repo_raw = parts.next()?.trim();
    if owner.is_empty() || repo_raw.is_empty() {
        return None;
    }
    let repo = repo_raw.strip_suffix(".git").unwrap_or(repo_raw);
    Some((owner.to_string(), repo.to_string()))
}

fn truncate_chars(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n[…truncated]", &s[..end])
}

// ─── On-disk cache ────────────────────────────────────────────────────

/// Compute the cache file path for a given project. Does not create the
/// directory or read the file.
pub fn cache_file_path(cache_dir: &Path, project_id: &str) -> PathBuf {
    cache_dir.join(format!("{project_id}.json"))
}

/// Best-effort cache read. Returns `None` for any failure (missing file,
/// unreadable, malformed JSON) — callers treat "no cache" uniformly,
/// regardless of cause.
pub fn cached_repo(cache_dir: &Path, project_id: &str) -> Option<CachedRepo> {
    let path = cache_file_path(cache_dir, project_id);
    let raw = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&raw).ok()
}

/// Run the live GitHub fetch and persist the result to
/// `<cache_dir>/{project_id}.json`. Overwrites any existing cache file
/// for that project — the brief's manual-refresh path uses this same
/// function. The 8-second timeout is enforced by [`analyze`] via the
/// existing `TIMEOUT_SECS` const.
pub async fn fetch_and_cache(
    cache_dir: &Path,
    project_id: &str,
    url: &str,
) -> Result<CachedRepo> {
    let repo = analyze(url).await?;
    let cached = CachedRepo {
        repo,
        fetched_at: iso_now(),
    };
    write_cache(cache_dir, project_id, &cached)
        .context("could not persist GitHub fetch cache")?;
    Ok(cached)
}

fn write_cache(cache_dir: &Path, project_id: &str, cached: &CachedRepo) -> Result<()> {
    std::fs::create_dir_all(cache_dir)
        .with_context(|| format!("create cache dir {}", cache_dir.display()))?;
    let path = cache_file_path(cache_dir, project_id);
    let json = serde_json::to_string_pretty(cached).context("serialize cache entry")?;
    std::fs::write(&path, json)
        .with_context(|| format!("write cache file {}", path.display()))?;
    Ok(())
}

/// Same format as `projects::now_iso` — a Unix-epoch seconds string with
/// a trailing `s`. Project convention; not a real ISO-8601 timestamp.
fn iso_now() -> String {
    let d = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    format!("{}s", d.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_https_url() {
        assert_eq!(
            parse_github_url("https://github.com/anthropics/claude-code"),
            Some(("anthropics".into(), "claude-code".into()))
        );
    }

    #[test]
    fn parses_url_with_trailing_slash() {
        assert_eq!(
            parse_github_url("https://github.com/anthropics/claude-code/"),
            Some(("anthropics".into(), "claude-code".into()))
        );
    }

    #[test]
    fn parses_url_with_tree_path() {
        assert_eq!(
            parse_github_url("https://github.com/anthropics/claude-code/tree/main/src"),
            Some(("anthropics".into(), "claude-code".into()))
        );
    }

    #[test]
    fn parses_dot_git_suffix() {
        assert_eq!(
            parse_github_url("https://github.com/anthropics/claude-code.git"),
            Some(("anthropics".into(), "claude-code".into()))
        );
    }

    #[test]
    fn parses_bare_github_paste() {
        assert_eq!(
            parse_github_url("github.com/anthropics/claude-code"),
            Some(("anthropics".into(), "claude-code".into()))
        );
    }

    #[test]
    fn parses_ssh_url() {
        assert_eq!(
            parse_github_url("git@github.com:anthropics/claude-code.git"),
            Some(("anthropics".into(), "claude-code".into()))
        );
    }

    #[test]
    fn rejects_non_github() {
        assert!(parse_github_url("https://gitlab.com/foo/bar").is_none());
        assert!(parse_github_url("not a url at all").is_none());
        assert!(parse_github_url("https://github.com/only-owner").is_none());
    }

    #[test]
    fn builds_description_combines_both_sources() {
        let out = build_description("A short repo tagline.", Some("# Hello\n\nReadme body."));
        assert!(out.contains("A short repo tagline."));
        assert!(out.contains("--- README ---"));
        assert!(out.contains("Readme body."));
    }

    #[test]
    fn builds_description_handles_missing_readme() {
        let out = build_description("Just a tagline.", None);
        assert_eq!(out, "Just a tagline.");
    }

    #[test]
    fn builds_description_handles_missing_upstream() {
        let out = build_description("", Some("# Title"));
        assert!(out.starts_with("--- README ---"));
        assert!(out.contains("# Title"));
    }

    // ─── Cache primitives ─────────────────────────────────────────────

    use std::sync::atomic::{AtomicU64, Ordering};

    fn fresh_cache_dir(label: &str) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let path = std::env::temp_dir().join(format!("ghcache_{label}_{pid}_{n}"));
        let _ = std::fs::remove_dir_all(&path);
        path
    }

    fn cleanup(p: &Path) {
        let _ = std::fs::remove_dir_all(p);
    }

    fn fake_cached_repo() -> CachedRepo {
        CachedRepo {
            repo: AnalyzedRepo {
                name: "foo".into(),
                description: "A test repo.".into(),
                default_branch: "main".into(),
                html_url: "https://github.com/example/foo".into(),
            },
            fetched_at: "1700000000s".into(),
        }
    }

    #[test]
    fn cache_file_path_includes_project_id_and_json_ext() {
        let dir = Path::new("/tmp/cache");
        let p = cache_file_path(dir, "proj_42");
        assert_eq!(p, Path::new("/tmp/cache/proj_42.json"));
    }

    #[test]
    fn cached_repo_returns_none_when_missing() {
        let dir = fresh_cache_dir("missing");
        assert!(cached_repo(&dir, "proj_does_not_exist").is_none());
    }

    #[test]
    fn cached_repo_round_trips_through_disk() {
        let dir = fresh_cache_dir("roundtrip");
        let original = fake_cached_repo();
        write_cache(&dir, "proj_1", &original).expect("write cache");

        let loaded = cached_repo(&dir, "proj_1").expect("cache should load");
        assert_eq!(loaded.repo.name, original.repo.name);
        assert_eq!(loaded.repo.description, original.repo.description);
        assert_eq!(loaded.repo.default_branch, original.repo.default_branch);
        assert_eq!(loaded.repo.html_url, original.repo.html_url);
        assert_eq!(loaded.fetched_at, original.fetched_at);

        cleanup(&dir);
    }

    #[test]
    fn cached_repo_returns_none_on_malformed_json() {
        let dir = fresh_cache_dir("malformed");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(cache_file_path(&dir, "proj_bad"), "{not json}").unwrap();
        assert!(cached_repo(&dir, "proj_bad").is_none());
        cleanup(&dir);
    }

    #[test]
    fn write_cache_creates_missing_directory() {
        let dir = fresh_cache_dir("create_dir");
        // Directory intentionally does not exist yet.
        assert!(!dir.exists());
        write_cache(&dir, "proj_x", &fake_cached_repo()).expect("write should succeed");
        assert!(dir.exists());
        assert!(cache_file_path(&dir, "proj_x").is_file());
        cleanup(&dir);
    }

    #[test]
    fn write_cache_overwrites_existing_entry() {
        let dir = fresh_cache_dir("overwrite");
        let mut first = fake_cached_repo();
        first.fetched_at = "1000s".into();
        write_cache(&dir, "proj_y", &first).unwrap();

        let mut second = fake_cached_repo();
        second.fetched_at = "2000s".into();
        second.repo.description = "Newer description.".into();
        write_cache(&dir, "proj_y", &second).unwrap();

        let loaded = cached_repo(&dir, "proj_y").expect("load after overwrite");
        assert_eq!(loaded.fetched_at, "2000s");
        assert_eq!(loaded.repo.description, "Newer description.");

        cleanup(&dir);
    }
}
