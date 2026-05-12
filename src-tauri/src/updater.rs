use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};

const RELEASES_API_URL: &str =
    "https://api.github.com/repos/Lakshmiyadav65/PromptEnhancer/releases/latest";
const USER_AGENT: &str = "PromptForge-UpdateCheck";
const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Deserialize)]
struct GitHubRelease {
    tag_name: String,
    html_url: String,
    body: Option<String>,
    prerelease: bool,
    draft: bool,
}

#[derive(Serialize)]
pub struct UpdateInfo {
    pub current_version: String,
    pub latest_version: String,
    pub update_available: bool,
    pub release_url: String,
    pub release_notes: Option<String>,
}

#[tauri::command]
pub async fn check_for_updates() -> std::result::Result<UpdateInfo, String> {
    do_check().await.map_err(|e| format!("{e:#}"))
}

async fn do_check() -> Result<UpdateInfo> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .user_agent(USER_AGENT)
        .build()
        .context("could not build HTTP client")?;

    let response = client
        .get(RELEASES_API_URL)
        .send()
        .await
        .context("could not reach GitHub releases API")?;

    if !response.status().is_success() {
        return Err(anyhow!(
            "GitHub releases API returned {}",
            response.status()
        ));
    }

    let release: GitHubRelease = response
        .json()
        .await
        .context("could not parse GitHub release JSON")?;

    if release.draft || release.prerelease {
        return Err(anyhow!(
            "latest GitHub release is a draft/prerelease — skipping"
        ));
    }

    let latest = release.tag_name.trim_start_matches('v').to_string();
    let update_available = is_newer(&latest, CURRENT_VERSION);

    Ok(UpdateInfo {
        current_version: CURRENT_VERSION.to_string(),
        latest_version: latest,
        update_available,
        release_url: release.html_url,
        release_notes: release.body,
    })
}

/// True if `latest` is strictly greater than `current` under naive semver
/// (split on `.`, parse u64s, compare component-wise).
fn is_newer(latest: &str, current: &str) -> bool {
    let parse = |s: &str| -> Vec<u64> {
        s.split('.')
            .map(|p| p.split('-').next().unwrap_or(""))
            .filter_map(|p| p.parse().ok())
            .collect()
    };
    let l = parse(latest);
    let c = parse(current);
    for i in 0..l.len().max(c.len()) {
        let lv = l.get(i).copied().unwrap_or(0);
        let cv = c.get(i).copied().unwrap_or(0);
        if lv > cv {
            return true;
        }
        if lv < cv {
            return false;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::is_newer;

    #[test]
    fn version_compare() {
        assert!(is_newer("0.2.0", "0.1.1"));
        assert!(is_newer("0.1.2", "0.1.1"));
        assert!(is_newer("1.0.0", "0.9.99"));
        assert!(!is_newer("0.1.1", "0.1.1"));
        assert!(!is_newer("0.1.0", "0.1.1"));
        assert!(is_newer("0.1.10", "0.1.2"));
    }
}
