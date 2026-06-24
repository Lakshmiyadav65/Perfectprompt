//! Project directory scanner. Produces a structured [`ProjectSummary`]
//! from the manifests and file layout of a project on disk.
//!
//! The summary is intentionally deterministic — no LLM call, no version
//! lookup, no network. Detection is driven by:
//!
//! - `Cargo.toml` → Rust + Tauri version (regex over the manifest text)
//! - `package.json` → React / Vue / Svelte / Next.js + their major versions
//! - lockfiles → pnpm / yarn / npm package manager
//! - file layout heuristics → conventions (integration-tests folder,
//!   co-located Jest tests, Tailwind config)
//!
//! Pure helpers (`detect_stack_from`, `detect_tooling_from`,
//! `extract_tauri_version`, `extract_major_version`, `strip_code_blocks`,
//! `collapse_whitespace`, `truncate_at_sentence`) take in-memory inputs
//! so they can be unit-tested without touching the filesystem. The one
//! filesystem-bound entry point is `scan_project_summary(path)`.

use std::fs;
use std::path::Path;
use std::sync::OnceLock;

use regex::Regex;
use serde_json::Value;

/// Hard cap on number of file-layout entries. Keeps the layout from
/// dominating the `<context>` block at assembly time (Step 4 still has
/// its own 2000-char ceiling).
const FILE_LAYOUT_MAX: usize = 30;

/// Hard cap on README excerpt length, in chars. Step 4 may truncate
/// further to fit the `<context>` ceiling.
const README_EXCERPT_MAX: usize = 500;

/// Maximum number of detected conventions to surface (brief §"What
/// project_scan needs to produce" — "Up to 4 detected conventions").
const CONVENTIONS_MAX: usize = 4;

/// Directory entries skipped during the file-layout walk. Build output,
/// vendored deps, IDE state, and version-control internals.
const SKIP_ENTRIES: &[&str] = &[
    "node_modules", ".git", "target", "dist", "build", ".next", ".nuxt",
    "__pycache__", "vendor", ".venv", "venv", "out", ".cache",
    ".idea", ".vscode", "coverage",
];

/// Structured scan result. Each field is independently optional — an
/// empty String / empty Vec means "nothing detected." Callers (Step 4's
/// `build_context_block`) decide which fields to surface.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectSummary {
    pub stack: String,
    pub tooling: String,
    pub conventions: String,
    pub file_layout: Vec<String>,
    pub readme_excerpt: String,
}

impl ProjectSummary {
    /// True when every field is blank — `scan_project_summary` uses this
    /// to decide whether to return `None`.
    pub fn is_empty(&self) -> bool {
        self.stack.is_empty()
            && self.tooling.is_empty()
            && self.conventions.is_empty()
            && self.file_layout.is_empty()
            && self.readme_excerpt.is_empty()
    }
}

/// Lockfile family. Drives the package-manager surface in `tooling`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum LockfileKind {
    Pnpm,
    Yarn,
    Npm,
    None,
}

impl LockfileKind {
    fn detect(dir: &Path) -> Self {
        if dir.join("pnpm-lock.yaml").is_file() {
            LockfileKind::Pnpm
        } else if dir.join("yarn.lock").is_file() {
            LockfileKind::Yarn
        } else if dir.join("package-lock.json").is_file() {
            LockfileKind::Npm
        } else {
            LockfileKind::None
        }
    }
}

/// Scan a project directory and build a [`ProjectSummary`].
///
/// Returns `None` when:
/// - the path is missing or not a directory, OR
/// - the directory has no detectable signal (no manifests, no notable
///   files, no README, no convention markers).
pub fn scan_project_summary(path: &Path) -> Option<ProjectSummary> {
    if !path.is_dir() {
        return None;
    }

    let cargo_toml = fs::read_to_string(path.join("Cargo.toml")).ok();
    let package_json_raw = fs::read_to_string(path.join("package.json")).ok();
    let package_json: Option<Value> = package_json_raw
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok());

    let lockfile = LockfileKind::detect(path);
    let file_layout = collect_file_layout(path);
    let readme_excerpt = extract_readme_excerpt(path);

    let stack = detect_stack_from(cargo_toml.as_deref(), package_json.as_ref());
    let tooling = detect_tooling_from(cargo_toml.as_deref(), package_json.as_ref(), lockfile);
    let conventions = detect_conventions_from(path, &file_layout);

    let summary = ProjectSummary {
        stack,
        tooling,
        conventions,
        file_layout,
        readme_excerpt,
    };

    if summary.is_empty() {
        None
    } else {
        Some(summary)
    }
}

// ─── Stack detection ──────────────────────────────────────────────────

fn detect_stack_from(cargo_toml: Option<&str>, package_json: Option<&Value>) -> String {
    let mut groups: Vec<String> = Vec::new();

    if let Some(toml) = cargo_toml {
        let mut rust_group = String::from("Rust");
        if let Some(ver) = extract_tauri_version(toml) {
            rust_group.push_str(" + Tauri ");
            rust_group.push_str(&ver);
        } else if has_tauri_dep(toml) {
            rust_group.push_str(" + Tauri");
        }
        groups.push(rust_group);
    }

    if let Some(pkg) = package_json {
        let deps = pkg.get("dependencies");
        let mut frontend_parts: Vec<String> = Vec::new();
        for (key, label) in [
            ("react", "React"),
            ("vue", "Vue"),
            ("svelte", "Svelte"),
            ("next", "Next.js"),
        ] {
            if let Some(ver_str) = deps.and_then(|d| d.get(key)).and_then(|v| v.as_str()) {
                let major = extract_major_version(ver_str);
                if major.is_empty() {
                    frontend_parts.push(label.to_string());
                } else {
                    frontend_parts.push(format!("{label} {major}"));
                }
            }
        }
        if !frontend_parts.is_empty() {
            groups.push(frontend_parts.join(" + "));
        }
    }

    groups.join(", ")
}

fn extract_tauri_version(cargo_toml: &str) -> Option<String> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        // Matches both `tauri = "2.x.y"` and `tauri = { version = "2.x.y", ... }`.
        // The leading anchor is `^\s*` so dependency-table prefixes
        // (`[dependencies]\n...`) line up cleanly.
        Regex::new(
            r#"(?m)^\s*tauri\s*=\s*(?:"([^"]+)"|\{[^}]*?version\s*=\s*"([^"]+)")"#,
        )
        .expect("static tauri version regex compiles")
    });
    let caps = re.captures(cargo_toml)?;
    let raw = caps.get(1).or_else(|| caps.get(2))?.as_str();
    let major = extract_major_version(raw);
    if major.is_empty() {
        Some(raw.to_string())
    } else {
        Some(major)
    }
}

fn has_tauri_dep(cargo_toml: &str) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r#"(?m)^\s*tauri\s*="#).expect("static tauri-presence regex compiles")
    });
    re.is_match(cargo_toml)
}

/// Extract the leading major-version component from a semver-ish string.
/// Strips common range prefixes (`^`, `~`, `=`, `>`, `<`). Returns the
/// empty string for input that has no leading digit.
fn extract_major_version(ver: &str) -> String {
    let stripped = ver.trim_start_matches(|c: char| matches!(c, '^' | '~' | '=' | '>' | '<' | ' '));
    let major_end = stripped
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(stripped.len());
    stripped[..major_end].to_string()
}

// ─── Tooling detection ────────────────────────────────────────────────

fn detect_tooling_from(
    cargo_toml: Option<&str>,
    package_json: Option<&Value>,
    lockfile: LockfileKind,
) -> String {
    let mut parts: Vec<String> = Vec::new();

    if cargo_toml.is_some() {
        parts.push("cargo".to_string());
    }

    if package_json.is_some() {
        let pm = match lockfile {
            LockfileKind::Pnpm => "pnpm",
            LockfileKind::Yarn => "yarn",
            // Default to npm even when no lockfile is present — most JS
            // projects ship with the npm CLI as the assumed driver.
            _ => "npm",
        };
        parts.push(pm.to_string());
    }

    if let Some(pkg) = package_json {
        let dev = pkg.get("devDependencies");
        for tool in ["jest", "vitest", "mocha", "playwright", "cypress"] {
            if dev.and_then(|d| d.get(tool)).is_some() {
                parts.push(tool.to_string());
            }
        }
    }

    if let Some(toml) = cargo_toml {
        if toml.contains("[dev-dependencies]") {
            parts.push("cargo test".to_string());
        }
    }

    parts.join(", ")
}

// ─── Conventions detection ────────────────────────────────────────────

fn detect_conventions_from(dir: &Path, file_layout: &[String]) -> String {
    let mut conv: Vec<String> = Vec::new();

    if dir.join("src-tauri").join("tests").is_dir() {
        conv.push("Rust integration tests in src-tauri/tests".to_string());
    }

    let has_underscore_tests = dir.join("src").join("__tests__").is_dir();
    let has_test_files = file_layout.iter().any(|f| {
        f.ends_with(".test.tsx")
            || f.ends_with(".test.ts")
            || f.ends_with(".test.jsx")
            || f.ends_with(".test.js")
    });
    if has_underscore_tests || has_test_files {
        conv.push("Jest co-located tests".to_string());
    }

    for cfg in [
        "tailwind.config.js",
        "tailwind.config.ts",
        "tailwind.config.cjs",
        "tailwind.config.mjs",
    ] {
        if dir.join(cfg).is_file() {
            conv.push("Tailwind for styling".to_string());
            break;
        }
    }

    conv.truncate(CONVENTIONS_MAX);
    conv.join(", ")
}

// ─── File-layout walk ─────────────────────────────────────────────────

fn collect_file_layout(dir: &Path) -> Vec<String> {
    let mut depth0_dirs: Vec<String> = Vec::new();
    let mut depth1_dirs: Vec<String> = Vec::new();
    let mut top_files: Vec<String> = Vec::new();

    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };

    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if should_skip(&name) {
            continue;
        }
        let path = entry.path();
        if path.is_dir() {
            depth0_dirs.push(format!("{name}/"));
            // Walk one more level for the second tier.
            if let Ok(sub_entries) = fs::read_dir(&path) {
                for sub in sub_entries.flatten() {
                    let sub_name = sub.file_name().to_string_lossy().to_string();
                    if should_skip(&sub_name) {
                        continue;
                    }
                    if sub.path().is_dir() {
                        depth1_dirs.push(format!("{name}/{sub_name}/"));
                    }
                    // Depth-1 files are intentionally omitted — only the
                    // top-level notable files are surfaced (brief: "List
                    // directories first, then notable top-level files").
                }
            }
        } else if is_notable_top_file(&name) {
            top_files.push(name);
        }
    }

    depth0_dirs.sort();
    depth1_dirs.sort();
    top_files.sort();

    let mut all: Vec<String> = Vec::with_capacity(
        depth0_dirs.len() + depth1_dirs.len() + top_files.len(),
    );
    all.extend(depth0_dirs);
    all.extend(depth1_dirs);
    all.extend(top_files);
    all.truncate(FILE_LAYOUT_MAX);
    all
}

fn should_skip(name: &str) -> bool {
    if SKIP_ENTRIES.iter().any(|s| s.eq_ignore_ascii_case(name)) {
        return true;
    }
    // Hidden entries other than the explicit skip set. The skip list
    // already covers `.git`, `.vscode`, `.idea`, etc.; this filter
    // catches stray dotfiles like `.editorconfig`.
    name.starts_with('.')
}

fn is_notable_top_file(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "readme.md"
            | "readme"
            | "readme.txt"
            | "readme.rst"
            | "package.json"
            | "cargo.toml"
            | "pyproject.toml"
            | "requirements.txt"
            | "go.mod"
            | "gemfile"
            | "pom.xml"
            | "build.gradle"
            | "build.gradle.kts"
            | "composer.json"
            | "tsconfig.json"
            | "tauri.conf.json"
            | "vite.config.ts"
            | "vite.config.js"
            | "next.config.js"
            | "next.config.ts"
            | "tailwind.config.js"
            | "tailwind.config.ts"
            | "tailwind.config.cjs"
            | "tailwind.config.mjs"
            | ".env.example"
            | "license"
            | "license.md"
            | "license.txt"
            | "package-lock.json"
            | "pnpm-lock.yaml"
            | "yarn.lock"
    )
}

// ─── README excerpt ───────────────────────────────────────────────────

fn extract_readme_excerpt(dir: &Path) -> String {
    let raw = match read_readme_raw(dir) {
        Some(r) => r,
        None => return String::new(),
    };
    let stripped = strip_code_blocks(&raw);
    let no_headers: String = stripped
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n");
    let collapsed = collapse_whitespace(&no_headers);
    truncate_at_sentence(&collapsed, README_EXCERPT_MAX)
}

fn read_readme_raw(dir: &Path) -> Option<String> {
    // Case-insensitive lookup. The candidate list covers the canonical
    // spellings; we iterate directory entries and match lowercase.
    let entries = fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let lower = name.to_ascii_lowercase();
        if matches!(
            lower.as_str(),
            "readme.md" | "readme" | "readme.txt" | "readme.rst"
        ) {
            let p = entry.path();
            if p.is_file() {
                return fs::read_to_string(&p).ok();
            }
        }
    }
    None
}

fn strip_code_blocks(s: &str) -> String {
    let mut out = String::new();
    let mut in_block = false;
    for line in s.lines() {
        if line.trim_start().starts_with("```") {
            in_block = !in_block;
            continue;
        }
        if !in_block {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

fn collapse_whitespace(s: &str) -> String {
    let mut out = String::new();
    let mut prev_blank = false;
    for line in s.lines() {
        if line.trim().is_empty() {
            if !prev_blank {
                out.push('\n');
                prev_blank = true;
            }
        } else {
            out.push_str(line.trim_end());
            out.push('\n');
            prev_blank = false;
        }
    }
    out.trim().to_string()
}

/// Truncate at most `max` bytes on a char boundary, preferring the last
/// sentence terminator (`.`, `!`, `?`) when one lies in the back 40% of
/// the slice. Below that threshold we just cut on the char boundary —
/// stopping mid-sentence beats throwing away most of the excerpt.
fn truncate_at_sentence(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut cut = max;
    while cut > 0 && !s.is_char_boundary(cut) {
        cut -= 1;
    }
    let slice = &s[..cut];
    if let Some(idx) = slice.rfind(|c: char| matches!(c, '.' | '!' | '?')) {
        if idx >= max * 6 / 10 {
            return s[..=idx].trim_end().to_string();
        }
    }
    slice.trim_end().to_string()
}

// ─── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    // ---- Pure helpers ------------------------------------------------

    #[test]
    fn returns_none_for_missing_dir() {
        let p = PathBuf::from("/this/path/does/not/exist/xyz123");
        assert!(scan_project_summary(&p).is_none());
    }

    #[test]
    fn extract_major_version_strips_caret_and_tilde() {
        assert_eq!(extract_major_version("^19.1.0"), "19");
        assert_eq!(extract_major_version("~1.2.3"), "1");
        assert_eq!(extract_major_version("2.0.0"), "2");
        assert_eq!(extract_major_version(">=4.0.0"), "4");
        assert_eq!(extract_major_version("latest"), "");
        assert_eq!(extract_major_version(""), "");
    }

    #[test]
    fn extract_tauri_version_simple_string() {
        let toml = r#"[dependencies]
tauri = "2.1.0"
serde = "1"
"#;
        assert_eq!(extract_tauri_version(toml).as_deref(), Some("2"));
    }

    #[test]
    fn extract_tauri_version_inline_table() {
        let toml = r#"[dependencies]
tauri = { version = "2.3.0", features = ["tray-icon"] }
"#;
        assert_eq!(extract_tauri_version(toml).as_deref(), Some("2"));
    }

    #[test]
    fn extract_tauri_version_returns_none_when_absent() {
        let toml = r#"[dependencies]
serde = "1"
"#;
        assert!(extract_tauri_version(toml).is_none());
    }

    #[test]
    fn detect_stack_rust_only() {
        let toml = r#"[dependencies]
serde = "1"
"#;
        assert_eq!(detect_stack_from(Some(toml), None), "Rust");
    }

    #[test]
    fn detect_stack_rust_plus_tauri() {
        let toml = r#"[dependencies]
tauri = { version = "2", features = [] }
"#;
        assert_eq!(detect_stack_from(Some(toml), None), "Rust + Tauri 2");
    }

    #[test]
    fn detect_stack_react_plus_vite_only() {
        let pkg: Value = serde_json::from_str(
            r#"{ "dependencies": { "react": "^19.1.0", "react-dom": "^19.1.0" } }"#,
        )
        .unwrap();
        assert_eq!(detect_stack_from(None, Some(&pkg)), "React 19");
    }

    #[test]
    fn detect_stack_rust_tauri_plus_react() {
        let toml = r#"[dependencies]
tauri = { version = "2", features = [] }
"#;
        let pkg: Value =
            serde_json::from_str(r#"{ "dependencies": { "react": "^19.1.0" } }"#).unwrap();
        assert_eq!(
            detect_stack_from(Some(toml), Some(&pkg)),
            "Rust + Tauri 2, React 19"
        );
    }

    #[test]
    fn detect_stack_empty_when_nothing() {
        assert_eq!(detect_stack_from(None, None), "");
    }

    #[test]
    fn detect_tooling_cargo_and_npm() {
        let toml = r#"[dependencies]
"#;
        let pkg: Value = serde_json::from_str(r#"{ "name": "x" }"#).unwrap();
        assert_eq!(
            detect_tooling_from(Some(toml), Some(&pkg), LockfileKind::None),
            "cargo, npm"
        );
    }

    #[test]
    fn detect_tooling_pnpm_lockfile_wins_over_default_npm() {
        let pkg: Value = serde_json::from_str(r#"{ "name": "x" }"#).unwrap();
        assert_eq!(
            detect_tooling_from(None, Some(&pkg), LockfileKind::Pnpm),
            "pnpm"
        );
    }

    #[test]
    fn detect_tooling_picks_up_jest_and_vitest() {
        let pkg: Value = serde_json::from_str(
            r#"{ "devDependencies": { "jest": "29.0.0", "vitest": "1.0.0" } }"#,
        )
        .unwrap();
        assert_eq!(
            detect_tooling_from(None, Some(&pkg), LockfileKind::None),
            "npm, jest, vitest"
        );
    }

    #[test]
    fn detect_tooling_appends_cargo_test_when_dev_deps_present() {
        let toml = r#"[dependencies]
serde = "1"

[dev-dependencies]
tempfile = "3"
"#;
        assert_eq!(
            detect_tooling_from(Some(toml), None, LockfileKind::None),
            "cargo, cargo test"
        );
    }

    #[test]
    fn detect_tooling_empty_when_no_manifests() {
        assert_eq!(detect_tooling_from(None, None, LockfileKind::None), "");
    }

    // ---- Helpers ------------------------------------------------------

    #[test]
    fn strip_code_blocks_removes_fenced_sections() {
        let md = "Hello.\n```rust\nfn main() {}\n```\nAfter.";
        let out = strip_code_blocks(md);
        assert!(out.contains("Hello."));
        assert!(out.contains("After."));
        assert!(!out.contains("fn main"));
    }

    #[test]
    fn collapse_whitespace_collapses_repeated_blank_lines() {
        let s = "a\n\n\n\nb";
        assert_eq!(collapse_whitespace(s), "a\n\nb");
    }

    #[test]
    fn truncate_at_sentence_prefers_terminator() {
        let s = "First sentence. Second sentence. Third sentence is a longer one.";
        let out = truncate_at_sentence(s, 40);
        // The slice up to byte 40 is "First sentence. Second sentence. Third s".
        // The last `.` is at byte 31 (>= 40 * 6 / 10 = 24) so we cut there.
        assert!(out.ends_with('.'));
        assert!(out.len() <= 40);
    }

    #[test]
    fn truncate_at_sentence_short_input_unchanged() {
        let s = "Short.";
        assert_eq!(truncate_at_sentence(s, 100), "Short.");
    }

    #[test]
    fn truncate_at_sentence_handles_unicode() {
        // Naive byte slicing would panic mid-codepoint.
        let s = "❤❤❤❤❤❤❤❤❤❤";
        let out = truncate_at_sentence(s, 5);
        assert!(out.len() <= 5);
        assert!(s.starts_with(&out));
    }

    #[test]
    fn is_notable_top_file_matches_common_manifests() {
        assert!(is_notable_top_file("package.json"));
        assert!(is_notable_top_file("Cargo.toml"));
        assert!(is_notable_top_file("README.md"));
        assert!(is_notable_top_file("tailwind.config.ts"));
        assert!(!is_notable_top_file("random.txt"));
    }

    // ---- Filesystem-bound integration tests ---------------------------
    //
    // These create a small directory tree under the OS temp dir, exercise
    // `scan_project_summary` end-to-end, and clean up after themselves.

    fn fresh_tempdir(label: &str) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let path = std::env::temp_dir().join(format!("psum_{label}_{pid}_{n}"));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("create tempdir");
        path
    }

    fn write(p: &Path, body: &str) {
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).expect("create parent");
        }
        fs::write(p, body).expect("write file");
    }

    fn cleanup(p: &Path) {
        let _ = fs::remove_dir_all(p);
    }

    #[test]
    fn integration_rust_tauri_react_project() {
        let dir = fresh_tempdir("rust_tauri_react");

        write(
            &dir.join("Cargo.toml"),
            r#"[package]
name = "x"
[dependencies]
tauri = { version = "2.1.0", features = ["tray-icon"] }
serde = "1"
[dev-dependencies]
tempfile = "3"
"#,
        );
        write(
            &dir.join("package.json"),
            r#"{
  "name": "x",
  "dependencies": { "react": "^19.1.0", "react-dom": "^19.1.0" },
  "devDependencies": { "vitest": "1.0.0" }
}"#,
        );
        write(&dir.join("package-lock.json"), "{}");
        fs::create_dir_all(dir.join("src-tauri").join("tests")).unwrap();
        fs::create_dir_all(dir.join("src").join("components")).unwrap();
        write(
            &dir.join("README.md"),
            "# PerfectPrompt\n\nA system-tray prompt enhancer.\nPress a hotkey, get a better prompt.\n",
        );

        let s = scan_project_summary(&dir).expect("summary built");

        assert_eq!(s.stack, "Rust + Tauri 2, React 19");
        assert_eq!(s.tooling, "cargo, npm, vitest, cargo test");
        assert!(
            s.conventions.contains("Rust integration tests in src-tauri/tests"),
            "expected the src-tauri/tests convention, got: {:?}",
            s.conventions
        );
        assert!(
            s.file_layout.iter().any(|e| e == "src/"),
            "file_layout missing top-level src/: {:?}",
            s.file_layout
        );
        assert!(
            s.file_layout.iter().any(|e| e == "src/components/"),
            "file_layout missing second-level src/components/: {:?}",
            s.file_layout
        );
        assert!(
            s.file_layout.iter().any(|e| e == "Cargo.toml"),
            "file_layout missing Cargo.toml: {:?}",
            s.file_layout
        );
        assert!(s.readme_excerpt.contains("system-tray prompt enhancer"));
        assert!(!s.readme_excerpt.contains("# PerfectPrompt"), "headers should be stripped");

        cleanup(&dir);
    }

    #[test]
    fn integration_skips_node_modules_and_target() {
        let dir = fresh_tempdir("skip_noise");
        fs::create_dir_all(dir.join("node_modules").join("react")).unwrap();
        fs::create_dir_all(dir.join("target").join("debug")).unwrap();
        fs::create_dir_all(dir.join("src")).unwrap();
        write(
            &dir.join("Cargo.toml"),
            "[package]\nname = \"x\"\n[dependencies]\n",
        );

        let s = scan_project_summary(&dir).expect("summary built");
        assert!(!s.file_layout.iter().any(|e| e.starts_with("node_modules")));
        assert!(!s.file_layout.iter().any(|e| e.starts_with("target")));
        assert!(s.file_layout.iter().any(|e| e == "src/"));

        cleanup(&dir);
    }

    #[test]
    fn integration_caps_file_layout_at_30() {
        let dir = fresh_tempdir("cap");
        for i in 0..50 {
            fs::create_dir_all(dir.join(format!("d{i:02}"))).unwrap();
        }
        write(&dir.join("Cargo.toml"), "[package]\nname=\"x\"\n");

        let s = scan_project_summary(&dir).expect("summary built");
        assert!(
            s.file_layout.len() <= FILE_LAYOUT_MAX,
            "file_layout exceeded cap: {}",
            s.file_layout.len()
        );

        cleanup(&dir);
    }

    #[test]
    fn integration_detects_tailwind_convention() {
        let dir = fresh_tempdir("tailwind");
        write(&dir.join("tailwind.config.ts"), "export default {}");
        write(&dir.join("package.json"), r#"{ "name": "x" }"#);

        let s = scan_project_summary(&dir).expect("summary built");
        assert!(
            s.conventions.contains("Tailwind for styling"),
            "expected tailwind convention, got: {:?}",
            s.conventions
        );

        cleanup(&dir);
    }
}
