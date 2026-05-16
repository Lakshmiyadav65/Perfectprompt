use serde::{Deserialize, Serialize};

use crate::active_app::ActiveAppContext;
use crate::settings::AppClassificationSettings;

/// How PerfectPrompt should treat the active app for the upcoming
/// enhancement (FR-002 of the context-aware-enhancement feature).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppClassification {
    /// Skip the questionnaire popup and run the developer-direct path.
    Developer,
    /// Show the questionnaire popup before enhancing.
    General,
}

/// Default executable names treated as developer environments. Lower-case,
/// no path. Matched against `process_name` of the foreground window.
pub const DEFAULT_DEVELOPER_APPS: &[&str] = &[
    // VS Code family
    "code.exe",
    "code - insiders.exe",
    "codium.exe",
    // AI-first IDEs
    "cursor.exe",
    "windsurf.exe",
    "antigravity.exe",
    "codex.exe",
    "claude.exe", // Claude Code desktop
    // Terminals
    "windowsterminal.exe",
    "wt.exe",
    "cmd.exe",
    "powershell.exe",
    "pwsh.exe",
    "bash.exe",
    "git-bash.exe",
    "mintty.exe",
    "alacritty.exe",
    "wezterm-gui.exe",
    "wezterm.exe",
    // JetBrains family
    "idea64.exe",
    "idea.exe",
    "pycharm64.exe",
    "pycharm.exe",
    "webstorm64.exe",
    "webstorm.exe",
    "rubymine64.exe",
    "rider64.exe",
    "clion64.exe",
    "goland64.exe",
    "phpstorm64.exe",
    "datagrip64.exe",
    "studio64.exe", // Android Studio
    // Other editors
    "sublime_text.exe",
    "atom.exe",
    "notepad++.exe",
    "nvim.exe",
    "vim.exe",
    "gvim.exe",
    "neovide.exe",
    "zed.exe",
    "fleet.exe",
    // Container / dev tooling
    "docker desktop.exe",
];

/// Default executable names that should always show the questionnaire.
/// These take precedence only when `default_unknown_app_behavior` is
/// configured to skip — for unknown apps with the default setting we
/// route to general anyway, so this list is mostly used to help the UI
/// reason about classifications.
pub const DEFAULT_GENERAL_APPS: &[&str] = &[
    "chrome.exe",
    "msedge.exe",
    "brave.exe",
    "firefox.exe",
    "opera.exe",
    "arc.exe",
    "notepad.exe",
    "wordpad.exe",
    "winword.exe",
    "excel.exe",
    "powerpnt.exe",
    "onenote.exe",
    "outlook.exe",
    "thunderbird.exe",
    "notion.exe",
    "obsidian.exe",
    "evernote.exe",
    "slack.exe",
    "discord.exe",
    "teams.exe",
    "ms-teams.exe",
    "telegram.exe",
    "whatsapp.exe",
    "explorer.exe",
];

/// Window-title substrings (case-insensitive) that strongly suggest a
/// developer context even when the host is a browser. Used to upgrade
/// browser-hosted coding environments (Codespaces, Replit, etc.).
const BROWSER_DEV_TITLE_HINTS: &[&str] = &[
    "github.dev",
    "codespaces",
    "stackblitz",
    "codesandbox",
    "replit",
    "gitpod",
    "codepen",
    "jsfiddle",
    "vscode.dev",
    "claude code",
    "github copilot",
];

const BROWSER_PROCESSES: &[&str] = &[
    "chrome.exe",
    "msedge.exe",
    "brave.exe",
    "firefox.exe",
    "opera.exe",
    "arc.exe",
];

/// Classify an active-window snapshot using the configured app lists,
/// the built-in defaults, and a small heuristic for browser-hosted dev
/// environments. Falls back to `default_unknown_app_behavior` when the
/// app is unrecognised (FR-007).
pub fn classify(
    ctx: &ActiveAppContext,
    settings: &AppClassificationSettings,
) -> AppClassification {
    if ctx.is_empty() {
        // Active window detection failed (edge case) — keep the safer
        // questionnaire flow so the user is never surprised by silent
        // enhancement on unknown surfaces.
        return AppClassification::General;
    }

    let proc_lower = ctx.process_name.to_lowercase();
    let title_lower = ctx.window_title.to_lowercase();

    // 1. User overrides win over defaults.
    if list_matches(&settings.developer_apps, &proc_lower) {
        return AppClassification::Developer;
    }
    if list_matches(&settings.general_apps, &proc_lower) {
        return AppClassification::General;
    }

    // 2. Built-in developer list.
    if DEFAULT_DEVELOPER_APPS.iter().any(|a| *a == proc_lower) {
        return AppClassification::Developer;
    }

    // 3. Browser hosting a known coding workspace — upgrade to developer.
    //    Low-confidence titles still fall through to the unknown branch.
    if BROWSER_PROCESSES.iter().any(|p| *p == proc_lower)
        && BROWSER_DEV_TITLE_HINTS
            .iter()
            .any(|hint| title_lower.contains(hint))
    {
        return AppClassification::Developer;
    }

    // 4. Built-in general list.
    if DEFAULT_GENERAL_APPS.iter().any(|a| *a == proc_lower) {
        return AppClassification::General;
    }

    // 5. Unknown — defer to user preference.
    settings.default_unknown_app_behavior
}

fn list_matches(list: &[String], proc_lower: &str) -> bool {
    list.iter().any(|entry| entry.to_lowercase() == proc_lower)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(process: &str, title: &str) -> ActiveAppContext {
        ActiveAppContext {
            process_name: process.to_string(),
            executable_path: format!("C:/fake/{}", process),
            window_title: title.to_string(),
            pid: 1234,
        }
    }

    fn default_settings() -> AppClassificationSettings {
        AppClassificationSettings::default()
    }

    #[test]
    fn vscode_classified_as_developer() {
        let s = default_settings();
        assert_eq!(
            classify(&ctx("Code.exe", "perfectprompt - Visual Studio Code"), &s),
            AppClassification::Developer
        );
    }

    #[test]
    fn windows_terminal_classified_as_developer() {
        let s = default_settings();
        assert_eq!(
            classify(&ctx("WindowsTerminal.exe", "PowerShell"), &s),
            AppClassification::Developer
        );
        assert_eq!(
            classify(&ctx("powershell.exe", ""), &s),
            AppClassification::Developer
        );
    }

    #[test]
    fn notepad_classified_as_general() {
        let s = default_settings();
        assert_eq!(
            classify(&ctx("notepad.exe", "Untitled - Notepad"), &s),
            AppClassification::General
        );
    }

    #[test]
    fn chrome_with_normal_title_is_general() {
        let s = default_settings();
        assert_eq!(
            classify(&ctx("chrome.exe", "Gmail - Inbox"), &s),
            AppClassification::General
        );
    }

    #[test]
    fn chrome_hosting_codespaces_is_developer() {
        let s = default_settings();
        assert_eq!(
            classify(
                &ctx("chrome.exe", "perfectprompt - github.dev"),
                &s
            ),
            AppClassification::Developer
        );
    }

    #[test]
    fn unknown_app_defaults_to_general() {
        let s = default_settings();
        assert_eq!(
            classify(&ctx("SomeRandomApp.exe", "Random window"), &s),
            AppClassification::General
        );
    }

    #[test]
    fn unknown_app_respects_developer_default() {
        let mut s = default_settings();
        s.default_unknown_app_behavior = AppClassification::Developer;
        assert_eq!(
            classify(&ctx("SomeRandomApp.exe", "Random window"), &s),
            AppClassification::Developer
        );
    }

    #[test]
    fn user_override_promotes_app_to_developer() {
        let mut s = default_settings();
        s.developer_apps.push("notepad.exe".into());
        assert_eq!(
            classify(&ctx("notepad.exe", "Untitled - Notepad"), &s),
            AppClassification::Developer
        );
    }

    #[test]
    fn user_override_demotes_app_to_general() {
        let mut s = default_settings();
        s.general_apps.push("Code.exe".into());
        assert_eq!(
            classify(&ctx("Code.exe", "src/main.rs - Visual Studio Code"), &s),
            AppClassification::General
        );
    }

    #[test]
    fn empty_context_is_general() {
        let s = default_settings();
        assert_eq!(
            classify(&ActiveAppContext::default(), &s),
            AppClassification::General
        );
    }
}
