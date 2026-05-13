use std::fs;
use std::path::{Path, PathBuf};

/// Maximum total characters injected into the enhancement context. Keeps
/// us well under Groq's 12K-token-per-minute limit on the 70B model after
/// accounting for the meta-prompt (~3K tokens) and the user input.
const MAX_CONTEXT_CHARS: usize = 3000;

/// Maximum chars taken from any single file. Keeps a single huge README
/// from eating the whole budget and starving the manifest/structure data.
const MAX_PER_FILE_CHARS: usize = 1200;

/// Top-level entries to skip when listing project structure. These are
/// build artefacts and vendored dependencies — useless for prompt context
/// and often huge.
const SKIP_ENTRIES: &[&str] = &[
    "node_modules", ".git", "target", "dist", "build", ".next", ".nuxt",
    "__pycache__", "vendor", ".venv", "venv", ".env", "out", ".cache",
    ".idea", ".vscode", ".DS_Store", "Thumbs.db", "coverage",
];

/// Files we actively want to read because they describe what the project
/// IS (manifest) or HOW IT WORKS (readme). Matched case-insensitively.
const PRIORITY_FILES: &[&str] = &[
    "README.md", "README", "README.txt", "README.rst",
    "package.json", "Cargo.toml", "pyproject.toml", "requirements.txt",
    "go.mod", "Gemfile", "pom.xml", "build.gradle", "build.gradle.kts",
    "composer.json", "tsconfig.json", "tauri.conf.json",
];

/// Walk the project directory's top level and produce a curated text
/// summary suitable for injecting into the LLM enhancement context.
///
/// Returns `None` if the path is missing or unreadable — callers should
/// fall back to the metadata-only flow.
pub fn scan_project_dir(path: &Path) -> Option<String> {
    if !path.is_dir() {
        return None;
    }

    let mut out = String::new();
    let mut remaining = MAX_CONTEXT_CHARS;

    // 1) Priority files: README, manifest. Read content, capped per-file.
    for name in PRIORITY_FILES {
        if remaining < 200 {
            break;
        }
        if let Some(content) = read_priority_file(path, name, MAX_PER_FILE_CHARS) {
            let header = format!("\n--- {name} ---\n");
            if header.len() + content.len() > remaining {
                let allowed = remaining.saturating_sub(header.len() + 20);
                out.push_str(&header);
                out.push_str(&safe_truncate(&content, allowed));
                out.push_str("\n[…truncated]");
                remaining = 0;
                break;
            }
            out.push_str(&header);
            out.push_str(&content);
            remaining = remaining.saturating_sub(header.len() + content.len());
        }
    }

    // 2) Top-level directory listing (one level deep) so the agent knows
    //    the project's shape without us dumping every file.
    if remaining > 200 {
        let listing = top_level_listing(path);
        if !listing.is_empty() {
            let header = "\n--- Top-level structure ---\n";
            let block = format!("{header}{listing}");
            if block.len() <= remaining {
                out.push_str(&block);
            } else {
                out.push_str(header);
                out.push_str(&safe_truncate(&listing, remaining.saturating_sub(header.len() + 20)));
                out.push_str("\n[…truncated]");
            }
        }
    }

    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

fn read_priority_file(dir: &Path, name: &str, max_chars: usize) -> Option<String> {
    // Case-insensitive match — Windows is already case-insensitive but
    // we want to match on Linux/macOS too for the same code path.
    let target_lower = name.to_ascii_lowercase();
    let entries = fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let fname = entry.file_name();
        let fname_str = fname.to_string_lossy();
        if fname_str.to_ascii_lowercase() == target_lower {
            let p = entry.path();
            if !p.is_file() {
                continue;
            }
            let raw = fs::read_to_string(&p).ok()?;
            return Some(safe_truncate(&raw, max_chars));
        }
    }
    None
}

fn top_level_listing(dir: &Path) -> String {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return String::new(),
    };

    let mut dirs: Vec<String> = Vec::new();
    let mut files: Vec<String> = Vec::new();

    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if SKIP_ENTRIES.iter().any(|s| s.eq_ignore_ascii_case(&name)) {
            continue;
        }
        // Hidden files (other than ones already in the skip list) — keep
        // a small handful but don't flood the listing with them.
        if name.starts_with('.') && name.len() > 1 {
            continue;
        }
        let p: PathBuf = entry.path();
        if p.is_dir() {
            dirs.push(format!("{name}/"));
        } else {
            files.push(name);
        }
    }

    dirs.sort();
    files.sort();

    // Bound the listing so a wide directory doesn't dominate. 30 entries
    // total is enough to convey shape without ballooning the context.
    const MAX_ENTRIES: usize = 30;
    let mut all: Vec<String> = dirs.into_iter().chain(files.into_iter()).collect();
    let overflow = all.len().saturating_sub(MAX_ENTRIES);
    all.truncate(MAX_ENTRIES);
    let mut out = all.join("\n");
    if overflow > 0 {
        out.push_str(&format!("\n[…{overflow} more entries]"));
    }
    out
}

/// Truncate to at most `max` chars, on a char boundary. The standard
/// slice `&s[..max]` would panic mid-codepoint on non-ASCII.
fn safe_truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returns_none_for_missing_dir() {
        let p = PathBuf::from("/this/path/does/not/exist/xyz123");
        assert!(scan_project_dir(&p).is_none());
    }

    #[test]
    fn safe_truncate_handles_unicode() {
        // Each "❤" is 3 UTF-8 bytes. Truncating at byte 5 (mid-codepoint)
        // would panic with naive slicing.
        let s = "❤❤❤❤";
        let out = safe_truncate(s, 5);
        // Must not panic and must end on a char boundary.
        assert!(out.len() <= 5);
        assert!(s.starts_with(&out));
    }

    #[test]
    fn safe_truncate_no_change_when_short() {
        assert_eq!(safe_truncate("hello", 100), "hello");
    }
}
