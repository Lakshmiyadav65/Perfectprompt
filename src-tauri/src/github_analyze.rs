use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};

const API_BASE: &str = "https://api.github.com";
const RAW_BASE: &str = "https://raw.githubusercontent.com";
const USER_AGENT: &str = "promptforge-app";
const TIMEOUT_SECS: u64 = 8;

/// Maximum README chars stitched into the description. Keeps the form
/// from showing a wall of text when the user opens the modal.
const MAX_README_CHARS: usize = 1500;

#[derive(Debug, Serialize)]
pub struct AnalyzedRepo {
    pub name: String,
    pub description: String,
    pub default_branch: String,
    pub html_url: String,
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
}
