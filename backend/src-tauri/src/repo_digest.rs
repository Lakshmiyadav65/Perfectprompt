//! Project Knowledge — directory digester.
//!
//! Both entry points (local folder picker and GitHub URL paste) converge
//! to a directory path on disk and call [`digest_directory`]. The
//! engine walks the directory with the `ignore` crate (so `.gitignore`,
//! `.ignore`, and a project-specific `.perfectpromptignore` are
//! respected automatically), prioritises files (README first, then
//! manifests, then source, etc.), packs them into a budgeted text blob,
//! strips secrets, and renders the whole thing as a Repomix-style
//! `<repository_digest>` block.
//!
//! The output is a single `String` ([`RepoDigest::digest_text`]) that
//! gets stored on the active `Project` and injected into the LLM's
//! `<context>` block on every enhance/polish call.
//!
//! Architecture notes:
//!   - No vector DB, no embeddings, no background daemon. The 120 KB
//!     budget is enough to surface README + manifests + key source
//!     files for a typical project; mega-monorepos are out of scope.
//!   - `git2`/`libgit2` are intentionally NOT used. GitHub's codeload
//!     endpoint serves tarballs over plain HTTPS and `reqwest` (already
//!     a dependency) streams them just fine.
//!   - Hosted-path limitation: the Supabase edge function wraps
//!     `input_text` in `<input>` tags server-side, so the digest is
//!     effectively dropped on hosted calls in v1. Same caveat as the
//!     existing project_scan context. Lift by extending the edge
//!     function contract to accept a `context_block` field.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use flate2::read::GzDecoder;
use ignore::WalkBuilder;
use regex::Regex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tar::Archive;

// ─────────────────────────────────────────────────────────────────────
// Tuning constants
// ─────────────────────────────────────────────────────────────────────

/// Maximum digest size in bytes. 120 KB ≈ 30k tokens (using the
/// 4-chars-per-token approximation), leaving roughly 85k tokens of the
/// 128k Llama 3.3 70B context window for the system prompt, the user's
/// selected input, and the model's response. Tune up if eval shows
/// truncation hurts quality; tune down if context-injection latency
/// becomes a problem.
pub const DIGEST_MAX_BYTES: usize = 120_000;

/// Hard cap on a single file's contribution to the digest. Stops one
/// huge README from consuming the whole budget. Files larger than this
/// are truncated with a `[truncated]` marker; the path still shows in
/// the directory tree.
pub const PER_FILE_MAX_BYTES: usize = 30_000;

/// Maximum bytes a single README contributes to the digest (Project
/// Knowledge revamp, Layer 2). The README is high-signal but a 50 KB
/// README on a 120 KB budget eats 40 % of the space before any source
/// code is packed. Manifests (now Tier 1) and source files (Tier 5)
/// carry the higher per-byte signal for "the LLM cited a real symbol",
/// so we clamp README more aggressively than PER_FILE_MAX_BYTES.
pub const README_MAX_BYTES: usize = 15_000;

/// Maximum bytes the `<api_surface>` preamble contributes to the
/// digest. The preamble is a heuristic-extracted list of exported
/// classes, functions, and types from Tier 4/5 files — it goes at the
/// TOP of the digest so the LLM sees what symbols already exist before
/// reading file contents. 4 KB is enough for a roomy project; bigger
/// would just dilute attention.
pub const API_PREAMBLE_MAX_BYTES: usize = 4_000;

/// Maximum number of API-surface entries we'll render before truncating
/// with an "…" marker. Stops a generated-code file with 5 000 exports
/// from blowing the preamble budget on its own.
pub const API_PREAMBLE_MAX_ENTRIES: usize = 80;

/// Maximum GitHub tarball size we will download. Anything larger and
/// we abort the stream and surface a friendly error. 100 MB is more
/// than enough for a typical real project; mega-monorepos like
/// torvalds/linux are intentionally out of scope.
pub const GITHUB_TARBALL_MAX_BYTES: u64 = 100 * 1024 * 1024;

/// Approximation used for token-count estimation when rendering the
/// digest. 4 chars per token is correct for English prose and OK for
/// most code (code tends to tokenize slightly denser, so this is a
/// conservative under-estimate that errs on the side of leaving more
/// context-window headroom for the LLM response). Do NOT add a real
/// tokenizer for v1 — tiktoken-rs would add ~5 MB to the binary.
pub const APPROX_CHARS_PER_TOKEN: usize = 4;

/// User-agent for the codeload.github.com request. Required by GitHub
/// or the request is rejected.
const USER_AGENT: &str = "perfectprompt-app";

/// Timeout for the codeload tarball request. Generous because the
/// download is the long pole — 100 MB over a 5 Mb/s connection is
/// ~160 s.
const TARBALL_TIMEOUT_SECS: u64 = 180;

// ─────────────────────────────────────────────────────────────────────
// Public types
// ─────────────────────────────────────────────────────────────────────

/// What the engine produces. Serialised to JSON and persisted on the
/// `Project` alongside name/description/etc.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RepoDigest {
    /// The rendered `<repository_digest>...</repository_digest>` block.
    /// Already wrapped — pipeline.rs splices it directly into the
    /// `<context>` block without further wrapping.
    pub digest_text: String,
    pub token_count_estimate: usize,
    pub file_count: usize,
    pub elided_count: usize,
    pub source: DigestSource,
    /// ISO-8601 timestamp at digest time.
    pub fetched_at: String,
    /// SHA-256 hex of the digest_text bytes. Useful for change detection
    /// without diffing two giant strings.
    pub sha256: String,
}

/// Where the digest came from. Serialised with `tag = "kind"` so the
/// TypeScript side discriminates on `digest.source.kind === "local"`
/// vs `=== "github"`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case", tag = "kind", content = "value")]
pub enum DigestSource {
    Local {
        /// Absolute path the user picked.
        path: String,
    },
    Github {
        owner: String,
        repo: String,
        branch: String,
        html_url: String,
    },
}

/// Configuration knobs. Defaults match the module-level constants;
/// tests override per-test to exercise the budget enforcement without
/// having to materialise 120 KB of fixture data.
#[derive(Debug, Clone)]
pub struct DigestConfig {
    pub max_bytes: usize,
    pub per_file_max_bytes: usize,
}

impl Default for DigestConfig {
    fn default() -> Self {
        Self {
            max_bytes: DIGEST_MAX_BYTES,
            per_file_max_bytes: PER_FILE_MAX_BYTES,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────
// Skip / classification tables
// ─────────────────────────────────────────────────────────────────────

/// Directory names that are NEVER recursed into, regardless of
/// `.gitignore` presence. Catches the common build-artefact / vendored-
/// dependency patterns that bloat the digest without adding signal.
const HARDCODED_SKIP_DIRS: &[&str] = &[
    "node_modules",
    ".git",
    "target",
    "dist",
    "build",
    "out",
    ".next",
    ".nuxt",
    ".svelte-kit",
    ".cache",
    "__pycache__",
    "vendor",
    ".venv",
    "venv",
    "env",
    "coverage",
    ".idea",
    ".vscode",
    ".DS_Store",
    ".pytest_cache",
    ".mypy_cache",
    ".tox",
    "htmlcov",
    ".nyc_output",
    ".terraform",
    ".gradle",
];

/// File extensions whose content is NEVER included in the digest. The
/// path still appears in the directory_structure tree so the LLM knows
/// the file exists; only the bytes are skipped.
const BINARY_EXTENSIONS: &[&str] = &[
    // Images
    "png", "jpg", "jpeg", "gif", "bmp", "svg", "ico", "webp", "avif",
    // Fonts
    "ttf", "otf", "woff", "woff2", "eot",
    // Archives
    "zip", "tar", "gz", "bz2", "7z", "rar", "xz",
    // Media
    "mp4", "mov", "mp3", "wav", "ogg", "webm", "mkv", "flac", "pdf",
    // Compiled
    "o", "a", "so", "dll", "exe", "class", "jar", "wasm",
    // Misc binary
    "db", "sqlite",
];

/// Lockfile filenames. The path appears in the directory tree and the
/// `<file>` block is emitted, but the content is replaced with a single
/// placeholder line. Avoids 1+ MB of `yarn.lock` chewing the budget for
/// zero LLM signal.
const LOCKFILE_NAMES: &[&str] = &[
    "package-lock.json",
    "yarn.lock",
    "pnpm-lock.yaml",
    "bun.lockb",
    "Cargo.lock",
    "poetry.lock",
    "Pipfile.lock",
    "composer.lock",
    "Gemfile.lock",
    "go.sum",
];

/// Manifest filenames (Tier 2). Project root only — nested copies fall
/// into Tier 7.
const MANIFEST_NAMES: &[&str] = &[
    "Cargo.toml",
    "package.json",
    "pyproject.toml",
    "go.mod",
    "pom.xml",
    "composer.json",
    "Gemfile",
    "requirements.txt",
    "deno.json",
    "build.gradle",
    "build.gradle.kts",
];

/// Build/lint/TS configuration filenames (Tier 3). Glob-prefix matched.
const TIER3_PREFIXES: &[&str] = &[
    "tsconfig",
    "vite.config",
    "webpack.config",
    "rollup.config",
    ".eslintrc",
    ".prettierrc",
    "tailwind.config",
    "next.config",
    "nuxt.config",
    "svelte.config",
    "astro.config",
];

/// Source-directory roots. Files at any depth under one of these get
/// at least Tier 5 (or better if they're an entry point — see
/// [`is_entry_point`]).
const SOURCE_ROOTS: &[&str] = &[
    "src",
    // Some TypeScript / JS libraries (notably sindresorhus/got)
    // ship their core under `source/` rather than `src/`. Without
    // this entry those files fall to Tier 7 and lose budget races
    // against Tier 6 tests — leaving the digest source-code-empty
    // and api_surface unrendered. Measured directly via the
    // Phase 1 diagnostic on sindresorhus/got: 0/3 of the core
    // files (index.ts / options.ts / errors.ts) made it in until
    // this entry was added.
    "source",
    "lib",
    "app",
    "components",
    "pages",
    "routes",
    "services",
    "utils",
    "hooks",
    "contexts",
    "stores",
    "models",
    "views",
];

/// Test-directory roots and filename suffixes (Tier 6).
const TEST_DIRS: &[&str] = &["tests", "test", "__tests__", "spec"];

// ─────────────────────────────────────────────────────────────────────
// Tier classifier
// ─────────────────────────────────────────────────────────────────────

/// Assign a tier (1..=7) to a file based on its relative path. Lower
/// tiers are packed first. See module docs for the full ladder.
///
/// Project Knowledge revamp (Layer 2): manifests and README swapped
/// places. Manifests now win Tier 1 because `package.json` /
/// `Cargo.toml` tells the LLM more about stack + dependencies +
/// scripts per byte than any prose README can. README drops to Tier 2
/// AND gets clamped to `README_MAX_BYTES` in the packing loop.
pub(crate) fn tier_for(rel_path: &Path) -> u8 {
    let components: Vec<String> = rel_path
        .components()
        .map(|c| c.as_os_str().to_string_lossy().to_string())
        .collect();
    if components.is_empty() {
        return 7;
    }
    let depth = components.len();
    let leaf = components.last().unwrap();
    let leaf_lc = leaf.to_lowercase();

    // Tier 1 — package manifests at project root.
    if depth == 1 && MANIFEST_NAMES.iter().any(|m| *m == leaf.as_str()) {
        return 1;
    }

    // Tier 2 — top-level README files. Nested READMEs fall to Tier 4
    // below (they're often subsystem docs, less important than the
    // main entry point).
    let is_readme_name = matches!(
        leaf_lc.as_str(),
        "readme" | "readme.md" | "readme.rst" | "readme.txt"
    );
    if is_readme_name && depth == 1 {
        return 2;
    }

    // Tier 3 — build / lint / TS configuration at project root.
    if depth == 1 {
        for prefix in TIER3_PREFIXES {
            if leaf.starts_with(prefix) {
                return 3;
            }
        }
    }

    // Tier 4 — entry-point source files (specific paths the framework
    // ecosystem conventionally treats as entry points).
    if is_entry_point(&components) {
        return 4;
    }

    // Tier 6 — tests (checked BEFORE Tier 5 so test files under src/
    // still get classified as tests, not source).
    if is_test_path(&components, &leaf_lc) {
        return 6;
    }

    // Tier 5 — other source files. Any file whose first component is
    // a recognised source root.
    if SOURCE_ROOTS.iter().any(|root| components[0] == *root) {
        return 5;
    }

    // Tier 4 (nested case) — nested READMEs.
    if is_readme_name {
        return 4;
    }

    // Tier 7 — everything else.
    7
}

fn is_entry_point(components: &[String]) -> bool {
    let leaf = components.last().unwrap();
    if components.len() == 2 && components[0] == "src" {
        // src/main.{ext}, src/index.{ext}, src/lib.rs, src/app.{ext}
        let leaf_lc = leaf.to_lowercase();
        if leaf_lc.starts_with("main.")
            || leaf_lc.starts_with("index.")
            || leaf_lc.starts_with("app.")
            || leaf_lc == "lib.rs"
        {
            return true;
        }
    }
    if components.len() == 2 && components[0] == "cmd" && leaf.to_lowercase().starts_with("main.") {
        return true;
    }
    // Top-level entry-point names commonly used in Python / Node / etc.
    if components.len() == 1 {
        let leaf_lc = leaf.to_lowercase();
        if matches!(
            leaf_lc.as_str(),
            "main.py" | "app.py" | "manage.py" | "server.js" | "server.ts"
        ) {
            return true;
        }
    }
    false
}

fn is_test_path(components: &[String], leaf_lc: &str) -> bool {
    // Tests/ at any level
    if components.iter().any(|c| TEST_DIRS.iter().any(|t| *t == c.as_str())) {
        return true;
    }
    // *_test.{rs,go,py}
    if leaf_lc.ends_with("_test.rs")
        || leaf_lc.ends_with("_test.go")
        || leaf_lc.ends_with("_test.py")
    {
        return true;
    }
    // *.test.{ts,tsx,js,jsx} / *.spec.{ts,tsx,js,jsx,rs}
    if let Some(stem) = leaf_lc.rsplit_once('.') {
        let ext = stem.1;
        let head = stem.0;
        if matches!(ext, "ts" | "tsx" | "js" | "jsx" | "rs")
            && (head.ends_with(".test") || head.ends_with(".spec"))
        {
            return true;
        }
    }
    false
}

// ─────────────────────────────────────────────────────────────────────
// Extension / lockfile / env-file checks
// ─────────────────────────────────────────────────────────────────────

pub(crate) fn is_binary_extension(p: &Path) -> bool {
    let Some(ext) = p.extension().and_then(|e| e.to_str()) else {
        return false;
    };
    let ext_lc = ext.to_lowercase();
    BINARY_EXTENSIONS.iter().any(|b| *b == ext_lc.as_str())
}

pub(crate) fn is_lockfile(p: &Path) -> bool {
    let Some(name) = p.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    LOCKFILE_NAMES.iter().any(|l| *l == name)
}

pub(crate) fn is_env_file(p: &Path) -> bool {
    let Some(name) = p.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    // .env, .env.local, .env.production, .env.example, .env.development
    name == ".env" || name.starts_with(".env.")
}

/// Top-level README detector. Used by the packing loop to apply the
/// tighter README_MAX_BYTES clamp only to the root README, leaving
/// nested READMEs (`docs/README.md`) on the regular per-file budget.
fn is_top_level_readme(rel_path: &Path) -> bool {
    let comps: Vec<_> = rel_path.components().collect();
    if comps.len() != 1 {
        return false;
    }
    let Some(name) = rel_path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    matches!(
        name.to_lowercase().as_str(),
        "readme" | "readme.md" | "readme.rst" | "readme.txt"
    )
}

// ─────────────────────────────────────────────────────────────────────
// Secret stripping
// ─────────────────────────────────────────────────────────────────────

/// Compile-once regex table for the named-secret patterns. We
/// intentionally avoid Shannon-entropy / generic high-entropy scanning
/// in v1 — too many false positives on real source code (long base64
/// data URLs, minified JS, etc.). Sticking to named patterns means we
/// never redact something the user actually wants the LLM to see.
fn secret_patterns() -> &'static [(Regex, &'static str)] {
    static PATTERNS: OnceLock<Vec<(Regex, &'static str)>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        let raw: &[(&str, &str)] = &[
            (r"sk-[A-Za-z0-9]{20,}", "openai"),
            (r"gsk_[A-Za-z0-9]{20,}", "groq"),
            (r"sk-ant-[A-Za-z0-9_\-]{20,}", "anthropic"),
            (r"ghp_[A-Za-z0-9]{36}", "github_pat"),
            (r"gho_[A-Za-z0-9]{36}", "github_oauth"),
            (r"github_pat_[A-Za-z0-9_]{50,}", "github_fine_grained"),
            (r"AKIA[0-9A-Z]{16}", "aws"),
            (r"xoxb-[A-Za-z0-9\-]{20,}", "slack_bot"),
            (r"xoxp-[A-Za-z0-9\-]{20,}", "slack_user"),
            (r"AIza[0-9A-Za-z_\-]{35}", "google_api"),
            (
                r"eyJ[A-Za-z0-9_\-]{20,}\.[A-Za-z0-9_\-]{20,}\.[A-Za-z0-9_\-]{20,}",
                "jwt",
            ),
        ];
        raw.iter()
            .map(|(p, l)| (Regex::new(p).expect("static secret regex"), *l))
            .collect()
    })
}

/// Replace any named-secret matches in `content` with `[REDACTED]`.
/// Leaves surrounding text intact. Pure function, easily unit-tested.
pub(crate) fn redact_secrets(content: &str) -> String {
    let mut out = content.to_string();
    for (re, _label) in secret_patterns().iter() {
        out = re.replace_all(&out, "[REDACTED]").into_owned();
    }
    out
}

/// Redact `.env`-style files. Preserves comments (starting with `#`)
/// and blank lines. Every `KEY=VALUE` line becomes `KEY=***`. Lines
/// that don't match the KEY=VALUE shape are kept verbatim so the
/// surrounding structure (e.g. multi-line comments) survives.
pub(crate) fn redact_env_file(content: &str) -> String {
    let mut out = String::with_capacity(content.len());
    for (idx, line) in content.lines().enumerate() {
        if idx > 0 {
            out.push('\n');
        }
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            out.push_str(line);
            continue;
        }
        // Match KEY=VALUE — split on the FIRST '=' only so values
        // containing '=' (rare but possible) are still redacted whole.
        if let Some(eq_idx) = line.find('=') {
            let key = &line[..eq_idx];
            // Preserve any inline comment after the value
            // (KEY=value # comment), although we still redact the value.
            out.push_str(key);
            out.push_str("=***");
        } else {
            // Some .env tools support `export KEY=value` — defensive
            // handling for that and any other shape we don't recognise.
            out.push_str(line);
        }
    }
    out
}

/// File-reading helper. Reads UTF-8 text; non-UTF-8 returns Err and
/// the caller skips the file's content (path still shows in the tree).
/// Applies the appropriate redaction policy based on file name.
fn read_with_redaction(path: &Path) -> Result<String> {
    let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
    let text = String::from_utf8(bytes)
        .map_err(|_| anyhow!("file is not valid UTF-8"))?;
    if is_env_file(path) {
        Ok(redact_env_file(&text))
    } else {
        Ok(redact_secrets(&text))
    }
}

// ─────────────────────────────────────────────────────────────────────
// Walker
// ─────────────────────────────────────────────────────────────────────

/// Walk the directory and return relative paths of files to consider.
/// Uses the `ignore` crate so .gitignore, .ignore, and a custom
/// .perfectpromptignore are all respected. The hardcoded skip-dir list
/// is enforced via `filter_entry` so it applies even in repos that
/// have no .gitignore (e.g., a freshly-extracted tarball with no
/// `node_modules` exclusion).
fn walk_files(root: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    let walker = WalkBuilder::new(root)
        .add_custom_ignore_filename(".perfectpromptignore")
        // Allow hidden files so .env (with redaction) and .gitignore
        // itself are visible to the LLM. The explicit HARDCODED_SKIP_DIRS
        // filter below catches the dotfiles we DON'T want (.DS_Store,
        // .vscode, .idea, .git, etc.) for both files and directories.
        .hidden(false)
        // Apply .gitignore even when the directory isn't a git checkout
        // (e.g. a freshly-extracted tarball). Without this, the walker
        // only honours .gitignore files that live inside a recognisable
        // git working tree.
        .require_git(false)
        .git_ignore(true)
        .git_global(false)
        .git_exclude(false)
        .filter_entry(|entry| {
            let name = entry.file_name().to_string_lossy();
            !HARDCODED_SKIP_DIRS.iter().any(|skip| name == *skip)
        })
        .build();
    for result in walker {
        let entry = match result {
            Ok(e) => e,
            Err(_) => continue, // skip unreadable entries
        };
        // Only files. The walker yields directories too — we don't
        // need them; the directory-structure tree is reconstructed
        // from the file paths.
        if entry.file_type().map(|t| !t.is_file()).unwrap_or(true) {
            continue;
        }
        let path = entry.path();
        let rel = match path.strip_prefix(root) {
            Ok(p) => p.to_path_buf(),
            Err(_) => continue,
        };
        if rel.as_os_str().is_empty() {
            continue;
        }
        out.push(rel);
    }
    out
}

// ─────────────────────────────────────────────────────────────────────
// Render (directory tree + file blocks + elided summary)
// ─────────────────────────────────────────────────────────────────────

/// Tree node used to render the `<directory_structure>` block. Built
/// from the flat list of relative paths. BTreeMap so siblings are
/// sorted lexicographically — gives deterministic output.
#[derive(Default)]
struct TreeNode {
    children: BTreeMap<String, TreeNode>,
}

fn build_tree(paths: &[PathBuf]) -> TreeNode {
    let mut root = TreeNode::default();
    for p in paths {
        let mut node = &mut root;
        for comp in p.components() {
            let name = comp.as_os_str().to_string_lossy().to_string();
            node = node.children.entry(name).or_default();
        }
    }
    root
}

fn render_tree(node: &TreeNode, indent: usize, out: &mut String) {
    for (name, child) in &node.children {
        for _ in 0..indent {
            out.push_str("  ");
        }
        out.push_str(name);
        if !child.children.is_empty() {
            out.push('/');
        }
        out.push('\n');
        render_tree(child, indent + 1, out);
    }
}

/// Truncate a UTF-8 string to at most `max` bytes, snapping back to
/// the nearest char boundary so we never split a multi-byte codepoint.
fn safe_char_truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

fn estimate_tokens(text: &str) -> usize {
    text.len() / APPROX_CHARS_PER_TOKEN
}

// ─────────────────────────────────────────────────────────────────────
// API surface extraction (Project Knowledge revamp, Layer 2)
// ─────────────────────────────────────────────────────────────────────

/// Static regex table for symbol extraction. Single compile, reused
/// across every digest call. The patterns cover the most common
/// "exported / public" forms in JavaScript / TypeScript / Rust /
/// Python / Java. They're heuristic — false negatives are fine
/// (the file itself is still in the digest), but false positives
/// would dilute the high-signal preamble.
fn api_surface_patterns() -> &'static [Regex] {
    static PATTERNS: OnceLock<Vec<Regex>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        let raw: &[&str] = &[
            // TypeScript / JavaScript: `export class Foo`,
            // `export default function bar`, etc.
            r"(?m)^export\s+(?:default\s+)?(class|function|interface|type|const|enum)\s+([A-Za-z0-9_$]+)",
            // Rust: `pub struct Foo`, `pub fn bar`, `pub enum Mode`.
            r"(?m)^pub\s+(struct|enum|trait|fn|mod|type)\s+([A-Za-z0-9_]+)",
            // Bare top-level definitions (Python, classic JS, etc.).
            r"(?m)^(class|function|def)\s+([A-Za-z0-9_]+)",
            // `module.exports = Foo` (CommonJS).
            r"(?m)^module\.exports\s*=\s*([A-Za-z0-9_]+)",
        ];
        raw.iter()
            .map(|p| Regex::new(p).expect("static api-surface regex"))
            .collect()
    })
}

/// Walk `included_files`, run the heuristic symbol regexes, and
/// render an `<api_surface>...</api_surface>` block grouping
/// findings by file path. Only files whose tier is 4 or 5
/// (entry-point / general source) contribute — manifests and tests
/// would just add noise.
///
/// Returns the empty string when no symbols were found. Caps total
/// output at `API_PREAMBLE_MAX_BYTES` and entry count at
/// `API_PREAMBLE_MAX_ENTRIES`.
fn extract_api_surface(included_files: &[(PathBuf, String, bool, usize)]) -> String {
    let patterns = api_surface_patterns();
    // (path, entries) — preserve packing order so the LLM sees
    // entry-point files first.
    let mut per_file: Vec<(String, Vec<String>)> = Vec::new();
    let mut total_entries = 0usize;

    // Code-source file extensions. Used to widen api_surface
    // coverage to files in unconventional layouts (e.g. got's
    // `source/` directory) that the tier classifier puts in Tier 7
    // ("everything else") because they don't live under one of the
    // recognised SOURCE_ROOTS (`src/`, `lib/`, ...). Without this
    // widening, an entire project's exports can be invisible to
    // api_surface — exactly the bug we measured against
    // sindresorhus/got.
    const SOURCE_EXTENSIONS: &[&str] = &[
        "ts", "tsx", "js", "jsx", "mjs", "cjs", "rs", "py", "go",
        "java", "kt", "swift", "rb", "php", "cs", "scala",
    ];

    for (rel_path, content, _truncated, _orig) in included_files {
        let tier = tier_for(rel_path);
        let is_source_ext = rel_path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| {
                let lower = e.to_ascii_lowercase();
                SOURCE_EXTENSIONS.iter().any(|s| *s == lower.as_str())
            })
            .unwrap_or(false);
        // Tier 4 (entry-point) and Tier 5 (recognised source roots)
        // always contribute. Tier 7 ("everything else") contributes
        // ONLY when the extension says it's source code — so docs /
        // scripts / configs at unusual paths don't pollute the
        // preamble. Tier 6 (tests) is always excluded; the LLM
        // doesn't need to hear about test-internal symbols when
        // suggesting where to make a change.
        let include = matches!(tier, 4 | 5) || (tier == 7 && is_source_ext);
        if !include {
            continue;
        }
        let mut entries: Vec<String> = Vec::new();
        let mut seen: std::collections::HashSet<String> =
            std::collections::HashSet::new();

        for re in patterns {
            for caps in re.captures_iter(content) {
                if total_entries >= API_PREAMBLE_MAX_ENTRIES {
                    break;
                }
                // Each pattern's last capture is the SYMBOL NAME. The
                // first numeric capture (when present) is the KIND
                // (class / function / fn / struct / etc).
                let symbol = caps
                    .iter()
                    .skip(1)
                    .filter_map(|m| m.map(|x| x.as_str()))
                    .last()
                    .unwrap_or("");
                if symbol.is_empty() {
                    continue;
                }
                let kind = if caps.len() >= 3 {
                    caps.get(1).map(|m| m.as_str()).unwrap_or("")
                } else {
                    ""
                };
                let entry = if kind.is_empty() {
                    symbol.to_string()
                } else {
                    format!("{kind} {symbol}")
                };
                if !seen.contains(&entry) {
                    seen.insert(entry.clone());
                    entries.push(entry);
                    total_entries += 1;
                }
            }
            if total_entries >= API_PREAMBLE_MAX_ENTRIES {
                break;
            }
        }

        if !entries.is_empty() {
            let path_str = rel_path.to_string_lossy().replace('\\', "/");
            per_file.push((path_str, entries));
        }
        if total_entries >= API_PREAMBLE_MAX_ENTRIES {
            break;
        }
    }

    if per_file.is_empty() {
        return String::new();
    }

    // Markdown-flavoured rendering with `###` per-file headings and
    // bulleted symbols. The visual prominence matters: a flat
    // `path:\n  sym1, sym2` block buried near other context was
    // experimentally measured to produce vague "the parsing
    // function" outputs at level-3 specificity. The `### path`
    // heading + bullet list pattern bumps the same prompts to
    // level-4/5 because the LLM treats markdown structure as
    // authoritative.
    //
    // The 400-byte instructional footer ("How to use this index")
    // turns the preamble from a passive reference into a directive
    // — telling the LLM literally what to do with these paths.
    // Keep the body cap conservative so the footer always fits
    // (API_PREAMBLE_MAX_BYTES - ~400 = ~3_600 body bytes).
    const FOOTER: &str = concat!(
        "\n## How to use this index\n",
        "When the user asks to modify or extend any of the classes, ",
        "functions, or types listed above, your enhanced prompt MUST ",
        "reference the file path AND the specific symbol by name. Do ",
        "not use vague phrases like \"the relevant module\" or \"the ",
        "appropriate file\" — the path is right here. This applies to ",
        "every prompt where a digest is present, including one-line ",
        "tweaks (a timeout change still goes into a specific file).\n",
    );
    let body_budget = API_PREAMBLE_MAX_BYTES.saturating_sub(FOOTER.len() + 64);

    let mut out = String::with_capacity(2 * 1024);
    out.push_str("<api_surface>\n");
    out.push_str(
        "## File-path index (use these paths verbatim when suggesting changes)\n\n",
    );
    let mut truncated = false;
    for (path, entries) in &per_file {
        // Each symbol gets its own bullet line. Sort within-file for
        // deterministic rendering; preserves the ordering across
        // digest rebuilds so SHA-based change detection (sha256
        // field on RepoDigest) stays stable when nothing actually
        // changed.
        let mut sorted_entries = entries.clone();
        sorted_entries.sort();
        let mut block = String::with_capacity(64 + path.len() + sorted_entries.len() * 24);
        block.push_str(&format!("### {path}\n"));
        for sym in &sorted_entries {
            block.push_str(&format!("- {sym}\n"));
        }
        block.push('\n');
        if out.len() + block.len() > body_budget {
            truncated = true;
            break;
        }
        out.push_str(&block);
    }
    if truncated {
        out.push_str("…\n\n");
    }
    out.push_str(FOOTER);
    out.push_str("</api_surface>");
    out
}

fn iso_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Minimal ISO-8601 — same approach as enhancement_history::iso_now.
    // Avoids pulling chrono just for one timestamp.
    let days = (secs / 86_400) as i64;
    let secs_of_day = secs % 86_400;
    let hh = (secs_of_day / 3600) as u32;
    let mm = ((secs_of_day % 3600) / 60) as u32;
    let ss = (secs_of_day % 60) as u32;
    let (y, mo, d) = civil_from_days(days);
    format!("{y:04}-{mo:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}Z")
}

/// Days since 1970-01-01 → (year, month, day). Howard Hinnant's
/// civil-from-days algorithm. Same implementation used in
/// enhancement_history.rs.
fn civil_from_days(days: i64) -> (i32, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    let year = (y + if m <= 2 { 1 } else { 0 }) as i32;
    (year, m, d)
}

fn source_attr(source: &DigestSource) -> String {
    match source {
        DigestSource::Local { path } => format!("local://{path}"),
        DigestSource::Github { owner, repo, branch, .. } => {
            format!("github://{owner}/{repo}@{branch}")
        }
    }
}

/// Build the final `<repository_digest>...</repository_digest>` string.
/// Pure rendering — all the byte budgeting and file selection has
/// already happened by the time we get here.
struct RenderInputs<'a> {
    source: &'a DigestSource,
    fetched_at: &'a str,
    all_paths: &'a [PathBuf],
    files: &'a [(PathBuf, String, bool, usize)], // (path, content, truncated, original_bytes)
    elided: &'a [PathBuf],
    /// Project Knowledge revamp (Layer 2): high-signal preamble that
    /// lists the project's exported classes, functions, and types
    /// before the directory tree. The LLM reads this first and uses
    /// it to detect existing APIs (e.g. "Option.env() already exists,
    /// no need to add it from scratch"). Empty string when no symbols
    /// were extracted.
    api_surface: &'a str,
}

fn render_digest(inputs: RenderInputs<'_>) -> String {
    let mut out = String::with_capacity(8 * 1024);

    let included = inputs.files.len();
    let elided = inputs.elided.len();
    out.push_str(&format!(
        "<repository_digest source=\"{}\" fetched_at=\"{}\" files_included=\"{}\" files_elided=\"{}\">\n",
        source_attr(inputs.source),
        inputs.fetched_at,
        included,
        elided,
    ));

    // ── api_surface (Project Knowledge revamp, Layer 2) ──
    // Renders BEFORE directory_structure so the LLM sees what
    // symbols already exist before scanning file contents. Empty
    // when no exports were found (or none matched the heuristics).
    if !inputs.api_surface.is_empty() {
        out.push_str(inputs.api_surface);
        out.push('\n');
    }

    // ── directory_structure ──
    out.push_str("<directory_structure>\n");
    let tree = build_tree(inputs.all_paths);
    render_tree(&tree, 0, &mut out);
    out.push_str("</directory_structure>\n\n");

    // ── file blocks ──
    for (path, content, truncated, original_bytes) in inputs.files {
        let path_str = path.to_string_lossy().replace('\\', "/");
        if *truncated {
            out.push_str(&format!(
                "<file path=\"{}\" truncated=\"true\" original_bytes=\"{}\">\n",
                escape_xml_attr(&path_str),
                original_bytes,
            ));
        } else {
            out.push_str(&format!(
                "<file path=\"{}\">\n",
                escape_xml_attr(&path_str)
            ));
        }
        out.push_str(content);
        if !content.ends_with('\n') {
            out.push('\n');
        }
        if *truncated {
            out.push_str("[truncated]\n");
        }
        out.push_str("</file>\n\n");
    }

    // ── elided summary (optional) ──
    if !inputs.elided.is_empty() {
        out.push_str("<elided_files>\n");
        out.push_str(&format!(
            "{} more files not shown (token budget exceeded).\n",
            inputs.elided.len()
        ));
        // List the first ~10 elided paths so the LLM sees what's missing.
        let preview: Vec<String> = inputs
            .elided
            .iter()
            .take(10)
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .collect();
        out.push_str("Top elided paths: ");
        out.push_str(&preview.join(", "));
        if inputs.elided.len() > preview.len() {
            out.push_str(", ...");
        }
        out.push('\n');
        out.push_str("</elided_files>\n");
    }

    out.push_str("</repository_digest>");
    out
}

/// Minimal XML attribute escape — paths shouldn't contain `<` or `&`
/// in practice but we defend just in case to keep the digest XML
/// well-formed for the LLM's parsing.
fn escape_xml_attr(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('"', "&quot;")
}

// ─────────────────────────────────────────────────────────────────────
// Entry point — digest a directory on disk
// ─────────────────────────────────────────────────────────────────────

/// Digest a directory at `root`. The directory must already exist on
/// disk — for the GitHub-URL flow, the caller is responsible for
/// downloading + extracting first (see [`fetch_github_tarball`]).
pub fn digest_directory(
    root: &Path,
    source: DigestSource,
    cfg: &DigestConfig,
) -> Result<RepoDigest> {
    if !root.exists() {
        return Err(anyhow!("path does not exist: {}", root.display()));
    }
    if !root.is_dir() {
        return Err(anyhow!("path is not a directory: {}", root.display()));
    }

    // 1. Walk & classify.
    let all_paths = walk_files(root);

    // 2. Sort by (tier, path).
    let mut sorted: Vec<(PathBuf, u8)> = all_paths
        .iter()
        .map(|p| (p.clone(), tier_for(p)))
        .collect();
    sorted.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));

    // 3. Pack files into the byte budget.
    let mut files: Vec<(PathBuf, String, bool, usize)> = Vec::new();
    let mut elided: Vec<PathBuf> = Vec::new();
    let mut used_bytes: usize = 0;

    for (rel_path, _tier) in sorted {
        let abs_path = root.join(&rel_path);

        // Binary extensions: path stays in the tree, no <file> block.
        if is_binary_extension(&rel_path) {
            continue;
        }

        let (content, truncated, original_bytes) = if is_lockfile(&rel_path) {
            // Lockfile special case: emit a placeholder line instead
            // of the actual content.
            (
                "[lockfile contents elided]".to_string(),
                false,
                std::fs::metadata(&abs_path)
                    .map(|m| m.len() as usize)
                    .unwrap_or(0),
            )
        } else {
            // Try to read + redact.
            match read_with_redaction(&abs_path) {
                Ok(text) => {
                    let original_len = text.len();
                    // Project Knowledge revamp (Layer 2): apply the
                    // tighter README clamp first. Top-level READMEs
                    // are Tier 2 — see tier_for. Nested READMEs
                    // (`docs/README.md` etc.) fall through to the
                    // generic per_file_max_bytes path.
                    let is_root_readme = is_top_level_readme(&rel_path);
                    let cap = if is_root_readme {
                        cfg.per_file_max_bytes.min(README_MAX_BYTES)
                    } else {
                        cfg.per_file_max_bytes
                    };
                    if original_len > cap {
                        let truncated_text = safe_char_truncate(&text, cap);
                        (truncated_text, true, original_len)
                    } else {
                        (text, false, original_len)
                    }
                }
                Err(_) => {
                    // Non-UTF-8 or unreadable — listed in tree, no block.
                    continue;
                }
            }
        };

        // Approximate the byte cost of the rendered `<file>...</file>`
        // block. The exact format is fixed in render_digest; we only
        // need a close-enough estimate to know when to stop packing.
        let estimated_block_bytes =
            content.len() + rel_path.to_string_lossy().len() + 64;
        if used_bytes + estimated_block_bytes > cfg.max_bytes {
            elided.push(rel_path);
            continue;
        }

        used_bytes += estimated_block_bytes;
        files.push((rel_path, content, truncated, original_bytes));
    }

    let fetched_at = iso_now();
    // Layer 2: extract the API surface preamble from the packed
    // source files. Runs after packing so we only consider files
    // that actually made it into the budget — no point listing a
    // symbol the LLM can't see the body for.
    let api_surface = extract_api_surface(&files);
    let digest_text = render_digest(RenderInputs {
        source: &source,
        fetched_at: &fetched_at,
        all_paths: &all_paths,
        files: &files,
        elided: &elided,
        api_surface: &api_surface,
    });

    let token_count_estimate = estimate_tokens(&digest_text);
    let file_count = files.len();
    let elided_count = elided.len();
    let sha256 = {
        let mut hasher = Sha256::new();
        hasher.update(digest_text.as_bytes());
        format!("{:x}", hasher.finalize())
    };

    Ok(RepoDigest {
        digest_text,
        token_count_estimate,
        file_count,
        elided_count,
        source,
        fetched_at,
        sha256,
    })
}

// ─────────────────────────────────────────────────────────────────────
// GitHub tarball fetch + extract
// ─────────────────────────────────────────────────────────────────────

/// Download a public-repo tarball from codeload.github.com, extract
/// to `dest_parent/{repo}-{branch}/`, return the extracted directory
/// path. Aborts (and removes any partial extraction) if the streamed
/// download exceeds [`GITHUB_TARBALL_MAX_BYTES`].
///
/// `branch` MUST be the default-branch name the caller obtained from
/// [`crate::github_analyze::analyze_github_repo`] — do not hardcode
/// `main`; many older repos still default to `master`.
pub async fn fetch_github_tarball(
    owner: &str,
    repo: &str,
    branch: &str,
    dest_parent: &Path,
) -> Result<PathBuf> {
    // Wipe any prior extraction for this repo so the digest reflects
    // the freshly-downloaded snapshot. v1 has no incremental refresh.
    if dest_parent.exists() {
        let _ = std::fs::remove_dir_all(dest_parent);
    }
    std::fs::create_dir_all(dest_parent)
        .with_context(|| format!("create cache dir {}", dest_parent.display()))?;

    let url = format!(
        "https://codeload.github.com/{}/{}/tar.gz/refs/heads/{}",
        owner, repo, branch
    );
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(TARBALL_TIMEOUT_SECS))
        .user_agent(USER_AGENT)
        .build()
        .context("build tarball HTTP client")?;

    let resp = client
        .get(&url)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;

    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        // Friendly hint: v1 doesn't do PAT-auth so private repos look
        // identical to nonexistent ones from the client side.
        return Err(anyhow!(
            "Repository not found or private. v1 supports public repos only."
        ));
    }
    if !resp.status().is_success() {
        return Err(anyhow!(
            "GitHub codeload returned {} for {}/{}@{}",
            resp.status(),
            owner,
            repo,
            branch
        ));
    }

    // Stream into a Vec<u8> with size enforcement. We could stream
    // directly to disk but holding the tarball in memory is simpler
    // and 100 MB is well within reach.
    let mut buf: Vec<u8> = Vec::new();
    let mut stream = resp.bytes_stream();
    use futures_util::StreamExt;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("read tarball chunk")?;
        if buf.len() as u64 + chunk.len() as u64 > GITHUB_TARBALL_MAX_BYTES {
            // Don't leave half-downloaded bytes lying around.
            let _ = std::fs::remove_dir_all(dest_parent);
            return Err(anyhow!(
                "Repository too large (over {} MB). Use a local clone instead.",
                GITHUB_TARBALL_MAX_BYTES / (1024 * 1024)
            ));
        }
        buf.extend_from_slice(&chunk);
    }

    // Extract on the blocking pool — `tar` is sync.
    let dest_parent_owned = dest_parent.to_path_buf();
    let extracted_root = tokio::task::spawn_blocking(move || -> Result<PathBuf> {
        extract_tarball(&buf, &dest_parent_owned)
    })
    .await
    .context("tarball extraction task panicked")??;

    Ok(extracted_root)
}

fn extract_tarball(bytes: &[u8], dest_parent: &Path) -> Result<PathBuf> {
    let cursor = std::io::Cursor::new(bytes);
    let gz = GzDecoder::new(cursor);
    let mut archive = Archive::new(gz);
    // Path traversal defence — `tar` 0.4 has `set_overwrite(true)` and
    // honours `Entry::path()` which already strips parent-dir refs in
    // safe mode. We additionally constrain the unpack root.
    archive
        .unpack(dest_parent)
        .with_context(|| format!("unpack tarball into {}", dest_parent.display()))?;

    // GitHub wraps content in a single top-level directory named
    // `{repo}-{branch_or_sha}`. Find it and return its path.
    let entries: Vec<_> = std::fs::read_dir(dest_parent)
        .with_context(|| format!("read dest_parent {}", dest_parent.display()))?
        .flatten()
        .collect();
    let dir_entry = entries
        .into_iter()
        .find(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .ok_or_else(|| anyhow!("tarball extracted no directories"))?;
    Ok(dir_entry.path())
}

// ─────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;

    // Minimal in-crate temp-dir helper (kept here to avoid pulling the
    // `tempfile` crate just for tests). Creates a per-process unique
    // directory under std::env::temp_dir() and cleans up on Drop.
    mod tempdir_lite {
        use std::path::{Path, PathBuf};
        use std::sync::atomic::{AtomicU64, Ordering};
        use std::time::{SystemTime, UNIX_EPOCH};

        static COUNTER: AtomicU64 = AtomicU64::new(0);

        pub struct TempDir {
            path: PathBuf,
        }

        impl TempDir {
            pub fn new() -> Self {
                let nanos = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0);
                let n = COUNTER.fetch_add(1, Ordering::Relaxed);
                let path =
                    std::env::temp_dir().join(format!("pp-digest-{nanos}-{n}"));
                std::fs::create_dir_all(&path).expect("create tempdir");
                Self { path }
            }
            pub fn path(&self) -> &Path {
                &self.path
            }
        }

        impl Drop for TempDir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.path);
            }
        }
    }

    // ── tier_for ──────────────────────────────────────────────────

    #[test]
    fn tier_for_top_level_readme() {
        // Project Knowledge revamp: README dropped from Tier 1 to
        // Tier 2 so manifests (which carry dependency/script signal at
        // higher density per byte) get first crack at the budget.
        assert_eq!(tier_for(Path::new("README.md")), 2);
        assert_eq!(tier_for(Path::new("README")), 2);
        assert_eq!(tier_for(Path::new("README.rst")), 2);
        assert_eq!(tier_for(Path::new("README.txt")), 2);
    }

    #[test]
    fn tier_for_nested_readme_is_tier_4() {
        // Nested READMEs are subsystem docs — important but below the
        // root README, manifests, configs, and entry points.
        assert_eq!(tier_for(Path::new("docs/README.md")), 4);
    }

    #[test]
    fn tier_for_manifests() {
        // Project Knowledge revamp: manifests promoted to Tier 1.
        assert_eq!(tier_for(Path::new("Cargo.toml")), 1);
        assert_eq!(tier_for(Path::new("package.json")), 1);
        assert_eq!(tier_for(Path::new("pyproject.toml")), 1);
        assert_eq!(tier_for(Path::new("go.mod")), 1);
    }

    #[test]
    fn tier_for_build_configs() {
        assert_eq!(tier_for(Path::new("tsconfig.json")), 3);
        assert_eq!(tier_for(Path::new("tsconfig.node.json")), 3);
        assert_eq!(tier_for(Path::new("vite.config.ts")), 3);
        assert_eq!(tier_for(Path::new("tailwind.config.js")), 3);
    }

    #[test]
    fn tier_for_entry_points() {
        assert_eq!(tier_for(Path::new("src/main.rs")), 4);
        assert_eq!(tier_for(Path::new("src/lib.rs")), 4);
        assert_eq!(tier_for(Path::new("src/index.tsx")), 4);
        assert_eq!(tier_for(Path::new("src/app.tsx")), 4);
        assert_eq!(tier_for(Path::new("cmd/main.go")), 4);
        assert_eq!(tier_for(Path::new("main.py")), 4);
    }

    #[test]
    fn tier_for_other_source() {
        assert_eq!(tier_for(Path::new("src/utils/helpers.ts")), 5);
        assert_eq!(tier_for(Path::new("components/Button.tsx")), 5);
        assert_eq!(tier_for(Path::new("lib/parser.rs")), 5);
    }

    #[test]
    fn tier_for_source_directory_pattern() {
        // Regression: sindresorhus/got and other TS libs use
        // `source/` instead of `src/`. These files must classify
        // as Tier 5 so they pack ahead of Tier 6 tests — otherwise
        // the entire core of the library ends up elided. Verified
        // via the dump_got_digest_to_file ignored test: before this
        // fix, source/core/options.ts was elided; after, it lands
        // in the digest and contributes to api_surface.
        assert_eq!(tier_for(Path::new("source/core/index.ts")), 5);
        assert_eq!(tier_for(Path::new("source/core/options.ts")), 5);
        assert_eq!(tier_for(Path::new("source/core/errors.ts")), 5);
    }

    #[test]
    fn tier_for_tests() {
        assert_eq!(tier_for(Path::new("tests/integration_test.rs")), 6);
        assert_eq!(tier_for(Path::new("src/foo_test.rs")), 6);
        assert_eq!(tier_for(Path::new("src/foo.test.ts")), 6);
        assert_eq!(tier_for(Path::new("__tests__/Button.test.tsx")), 6);
    }

    #[test]
    fn tier_for_everything_else() {
        assert_eq!(tier_for(Path::new("docs/internal.md")), 7);
        assert_eq!(tier_for(Path::new("scripts/release.sh")), 7);
        assert_eq!(tier_for(Path::new("misc/notes.txt")), 7);
    }

    // ── is_binary_extension / is_lockfile / is_env_file ──────────

    #[test]
    fn is_binary_extension_recognises_images_fonts_pdf() {
        assert!(is_binary_extension(Path::new("logo.png")));
        assert!(is_binary_extension(Path::new("a/b/icon.ICO")));
        assert!(is_binary_extension(Path::new("font.ttf")));
        assert!(is_binary_extension(Path::new("doc.pdf")));
    }

    #[test]
    fn is_binary_extension_rejects_text_files() {
        assert!(!is_binary_extension(Path::new("src/main.rs")));
        assert!(!is_binary_extension(Path::new("README.md")));
        assert!(!is_binary_extension(Path::new("a.json")));
    }

    #[test]
    fn is_lockfile_recognises_known_names() {
        assert!(is_lockfile(Path::new("package-lock.json")));
        assert!(is_lockfile(Path::new("Cargo.lock")));
        assert!(is_lockfile(Path::new("yarn.lock")));
        assert!(!is_lockfile(Path::new("Cargo.toml")));
        assert!(!is_lockfile(Path::new("package.json")));
    }

    #[test]
    fn is_env_file_recognises_dotenv_variants() {
        assert!(is_env_file(Path::new(".env")));
        assert!(is_env_file(Path::new(".env.local")));
        assert!(is_env_file(Path::new(".env.production")));
        assert!(is_env_file(Path::new(".env.example")));
        assert!(!is_env_file(Path::new("env.txt")));
        assert!(!is_env_file(Path::new("README.md")));
    }

    // ── redact_env_file ──────────────────────────────────────────

    #[test]
    fn redact_env_file_masks_values_keeps_keys() {
        let input = "# Production keys\nGROQ_API_KEY=gsk_realkey1234567890\nPORT=3000\n\n# Backup\nSTRIPE_KEY=sk_test_abc";
        let expected = "# Production keys\nGROQ_API_KEY=***\nPORT=***\n\n# Backup\nSTRIPE_KEY=***";
        assert_eq!(redact_env_file(input), expected);
    }

    #[test]
    fn redact_env_file_preserves_comments_and_blank_lines() {
        let input = "# top\n\nFOO=bar\n# mid\nBAZ=qux\n";
        let out = redact_env_file(input);
        assert!(out.starts_with("# top\n\n"));
        assert!(out.contains("FOO=***"));
        assert!(out.contains("BAZ=***"));
        assert!(out.contains("# mid"));
    }

    // ── redact_secrets ───────────────────────────────────────────

    #[test]
    fn redact_secrets_openai_key() {
        let input = "the key is sk-1234567890abcdef1234567890abcdef test";
        let out = redact_secrets(input);
        assert!(out.contains("[REDACTED]"));
        assert!(!out.contains("sk-1234567890abcdef1234567890abcdef"));
    }

    #[test]
    fn redact_secrets_github_pat() {
        let input = "ghp_abcdefghijABCDEFGHIJ1234567890123456 my pat";
        let out = redact_secrets(input);
        assert!(out.contains("[REDACTED]"));
    }

    #[test]
    fn redact_secrets_aws_access_key() {
        let input = "AKIAIOSFODNN7EXAMPLE secret";
        let out = redact_secrets(input);
        assert!(out.contains("[REDACTED]"));
    }

    #[test]
    fn redact_secrets_slack_bot() {
        let input = "xoxb-1234567890-abcdefghij token";
        let out = redact_secrets(input);
        assert!(out.contains("[REDACTED]"));
    }

    #[test]
    fn redact_secrets_leaves_plain_text_alone() {
        let input = "regular text without any keys";
        let out = redact_secrets(input);
        assert_eq!(out, input);
        assert!(!out.contains("[REDACTED]"));
    }

    // ── digest_directory integration tests (tmpdir-based) ────────

    fn tmpdir() -> tempdir_lite::TempDir {
        tempdir_lite::TempDir::new()
    }

    fn write(path: &Path, content: &[u8]) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, content).unwrap();
    }

    #[test]
    fn render_digest_basic_shape() {
        let td = tmpdir();
        write(&td.path().join("README.md"), b"# Hello\nA simple project.");
        write(
            &td.path().join("src").join("main.rs"),
            b"fn main() { println!(\"hi\"); }",
        );

        let d = digest_directory(
            td.path(),
            DigestSource::Local {
                path: td.path().to_string_lossy().to_string(),
            },
            &DigestConfig::default(),
        )
        .unwrap();
        assert!(d.digest_text.starts_with("<repository_digest "));
        assert!(d.digest_text.contains("<directory_structure>"));
        assert!(d.digest_text.contains("README.md"));
        assert!(d.digest_text.contains("main.rs"));
        assert!(d.digest_text.contains("A simple project."));
        assert!(d.digest_text.contains("fn main()"));
        assert_eq!(d.file_count, 2);
        assert_eq!(d.elided_count, 0);
    }

    #[test]
    fn digest_respects_byte_budget() {
        // 10 files × 50 KB + small README should exceed 120 KB.
        let td = tmpdir();
        write(&td.path().join("README.md"), &b"a".repeat(1000));
        for i in 0..10 {
            let big = "x".repeat(50_000);
            write(&td.path().join("src").join(format!("big_{i}.rs")), big.as_bytes());
        }
        let d = digest_directory(
            td.path(),
            DigestSource::Local {
                path: td.path().to_string_lossy().to_string(),
            },
            &DigestConfig::default(),
        )
        .unwrap();
        // ~120 KB budget + some XML framing slop ≤ 130 KB.
        assert!(
            d.digest_text.len() <= DIGEST_MAX_BYTES + 10_000,
            "digest too large: {}",
            d.digest_text.len()
        );
        // At least half the big_*.rs files should have been elided.
        assert!(d.elided_count >= 5, "elided_count = {}", d.elided_count);
        // README is Tier 1 — must survive.
        assert!(d.digest_text.contains("README.md"));
    }

    #[test]
    fn digest_clamps_top_level_readme_to_readme_max_bytes() {
        // Layer 2: README is Tier 2, manifest is Tier 1, source is
        // Tier 5. A 50 KB README must NOT eat 40 % of the budget — it
        // gets clamped to README_MAX_BYTES (15 KB) regardless of the
        // per_file_max_bytes setting.
        let td = tmpdir();
        let big_readme = "r".repeat(50_000);
        write(&td.path().join("README.md"), big_readme.as_bytes());
        write(&td.path().join("package.json"), br#"{"name":"x"}"#);
        write(&td.path().join("src").join("main.rs"), b"fn main() {}");
        let d = digest_directory(
            td.path(),
            DigestSource::Local {
                path: td.path().to_string_lossy().to_string(),
            },
            &DigestConfig::default(),
        )
        .unwrap();
        assert!(
            d.digest_text.contains("<file path=\"README.md\""),
            "README must be packed"
        );
        assert!(
            d.digest_text.contains("truncated=\"true\""),
            "oversized README must be flagged as truncated"
        );
        assert!(
            d.digest_text.contains("original_bytes=\"50000\""),
            "truncation tag must record original size"
        );
        // Walk the rendered <file path="README.md"> block and verify
        // its content portion is <= README_MAX_BYTES. Skip the
        // attributes line; only the BODY counts toward the clamp.
        let start = d
            .digest_text
            .find("<file path=\"README.md\"")
            .expect("README block");
        let body_start = d.digest_text[start..]
            .find('\n')
            .map(|i| start + i + 1)
            .unwrap();
        let body_end = d.digest_text[body_start..]
            .find("</file>")
            .map(|i| body_start + i)
            .unwrap();
        let body_len = body_end - body_start;
        assert!(
            body_len <= README_MAX_BYTES + 64,
            "README body {} > README_MAX_BYTES {}",
            body_len,
            README_MAX_BYTES
        );
        // Manifest + source must still be present.
        assert!(d.digest_text.contains("<file path=\"package.json\""));
        assert!(d.digest_text.contains("<file path=\"src/main.rs\""));
    }

    #[test]
    fn digest_truncates_oversized_single_file() {
        let td = tmpdir();
        let cfg = DigestConfig {
            max_bytes: 500_000,
            per_file_max_bytes: 30_000,
        };
        let big = "y".repeat(80_000);
        write(&td.path().join("src").join("main.rs"), big.as_bytes());
        let d = digest_directory(
            td.path(),
            DigestSource::Local {
                path: td.path().to_string_lossy().to_string(),
            },
            &cfg,
        )
        .unwrap();
        assert!(d.digest_text.contains("truncated=\"true\""));
        assert!(d.digest_text.contains("original_bytes=\"80000\""));
        assert!(d.digest_text.contains("[truncated]"));
    }

    #[test]
    fn digest_respects_gitignore() {
        let td = tmpdir();
        write(&td.path().join(".gitignore"), b"secret-target.txt\n");
        write(&td.path().join("README.md"), b"hello");
        write(&td.path().join("src").join("main.rs"), b"fn main(){}");
        write(&td.path().join("secret-target.txt"), b"shh");
        let d = digest_directory(
            td.path(),
            DigestSource::Local { path: td.path().to_string_lossy().to_string() },
            &DigestConfig::default(),
        )
        .unwrap();
        // Tight check — the gitignored file's PATH must not appear as
        // a <file> block. Substring matching against the bare name is
        // unreliable because the .gitignore file's own content mentions
        // it as a rule.
        assert!(
            !d.digest_text.contains("<file path=\"secret-target.txt\""),
            "secret-target.txt should be excluded by .gitignore"
        );
        assert!(!d.digest_text.contains("shh"));
        assert!(d.digest_text.contains("README.md"));
        assert!(d.digest_text.contains("main.rs"));
    }

    #[test]
    fn digest_respects_perfectpromptignore() {
        let td = tmpdir();
        write(&td.path().join(".perfectpromptignore"), b"secret-target.txt\n");
        write(&td.path().join("README.md"), b"hello");
        write(&td.path().join("secret-target.txt"), b"shh");
        let d = digest_directory(
            td.path(),
            DigestSource::Local { path: td.path().to_string_lossy().to_string() },
            &DigestConfig::default(),
        )
        .unwrap();
        assert!(
            !d.digest_text.contains("<file path=\"secret-target.txt\""),
            ".perfectpromptignore should exclude the secret"
        );
        assert!(!d.digest_text.contains("shh"));
        assert!(d.digest_text.contains("README.md"));
    }

    #[test]
    fn digest_elides_lockfile_content() {
        let td = tmpdir();
        write(&td.path().join("package.json"), b"{\"name\":\"x\"}");
        // Synthetic 1 MB lockfile — its content must NOT land in the
        // digest verbatim.
        let big = "0".repeat(1_000_000);
        write(&td.path().join("package-lock.json"), big.as_bytes());
        let d = digest_directory(
            td.path(),
            DigestSource::Local { path: td.path().to_string_lossy().to_string() },
            &DigestConfig::default(),
        )
        .unwrap();
        assert!(d.digest_text.contains("package.json"));
        assert!(d.digest_text.contains("package-lock.json"));
        assert!(d.digest_text.contains("[lockfile contents elided]"));
        // The actual lockfile content must NOT be in the digest.
        assert!(
            !d.digest_text.contains(&"0".repeat(100)),
            "lockfile bytes leaked into digest"
        );
    }

    #[test]
    fn digest_lists_binary_in_tree_but_skips_content() {
        let td = tmpdir();
        write(&td.path().join("README.md"), b"hi");
        // Fake PNG bytes — content is irrelevant since we skip on extension.
        write(&td.path().join("logo.png"), &[0u8, 1, 2, 3, 4, 5]);
        write(&td.path().join("src").join("main.rs"), b"fn main(){}");
        let d = digest_directory(
            td.path(),
            DigestSource::Local { path: td.path().to_string_lossy().to_string() },
            &DigestConfig::default(),
        )
        .unwrap();
        // logo.png appears in the directory tree...
        assert!(d.digest_text.contains("logo.png"));
        // ...but has no <file path="logo.png"> block.
        assert!(!d.digest_text.contains("<file path=\"logo.png\""));
        assert!(d.digest_text.contains("<file path=\"README.md\""));
        assert!(d.digest_text.contains("<file path=\"src/main.rs\""));
    }

    #[test]
    fn digest_redacts_env_values() {
        let td = tmpdir();
        write(
            &td.path().join(".env"),
            b"GROQ_API_KEY=gsk_fakeButLooksReal1234567890abcdef\n",
        );
        let d = digest_directory(
            td.path(),
            DigestSource::Local { path: td.path().to_string_lossy().to_string() },
            &DigestConfig::default(),
        )
        .unwrap();
        assert!(d.digest_text.contains("GROQ_API_KEY=***"));
        assert!(!d.digest_text.contains("gsk_fakeButLooksReal"));
    }

    // ── API surface preamble (Project Knowledge revamp, Layer 2) ──

    #[test]
    fn api_surface_extracts_typescript_exports() {
        let td = tmpdir();
        write(
            &td.path().join("lib").join("option.js"),
            br#"export class Option {
  name() { return 1; }
  env(varName) { return varName; }
  default(val) { return val; }
}
export function createOption(flags) { return new Option(flags); }
"#,
        );
        write(&td.path().join("package.json"), br#"{"name":"x"}"#);
        let d = digest_directory(
            td.path(),
            DigestSource::Local { path: td.path().to_string_lossy().to_string() },
            &DigestConfig::default(),
        )
        .unwrap();
        assert!(
            d.digest_text.contains("<api_surface>"),
            "api_surface preamble missing: {}",
            &d.digest_text[..500.min(d.digest_text.len())]
        );
        assert!(d.digest_text.contains("class Option"));
        assert!(d.digest_text.contains("function createOption"));
        // The preamble must appear before <directory_structure>.
        let api_idx = d.digest_text.find("<api_surface>").unwrap();
        let tree_idx = d.digest_text.find("<directory_structure>").unwrap();
        assert!(api_idx < tree_idx, "api_surface must precede directory tree");
    }

    #[test]
    fn api_surface_extracts_rust_pubs() {
        let td = tmpdir();
        write(
            &td.path().join("src").join("lib.rs"),
            br#"pub struct Engine { name: String }
pub fn process(input: &str) -> Result<String, String> { Ok(input.into()) }
pub enum Mode { Casual, Formal }
"#,
        );
        write(&td.path().join("Cargo.toml"), b"[package]\nname=\"x\"");
        let d = digest_directory(
            td.path(),
            DigestSource::Local { path: td.path().to_string_lossy().to_string() },
            &DigestConfig::default(),
        )
        .unwrap();
        assert!(d.digest_text.contains("struct Engine"));
        assert!(d.digest_text.contains("fn process"));
        assert!(d.digest_text.contains("enum Mode"));
    }

    #[test]
    fn api_surface_omits_files_with_no_exports() {
        // A pure-data file (no class/fn/export) shouldn't appear in
        // the preamble at all, even though it's packed in the digest.
        let td = tmpdir();
        write(
            &td.path().join("src").join("constants.js"),
            b"const PI = 3.14;\nconst E = 2.71;\n",
        );
        write(&td.path().join("package.json"), br#"{"name":"x"}"#);
        let d = digest_directory(
            td.path(),
            DigestSource::Local { path: td.path().to_string_lossy().to_string() },
            &DigestConfig::default(),
        )
        .unwrap();
        // The file is packed but the preamble shouldn't list it.
        assert!(d.digest_text.contains("<file path=\"src/constants.js\""));
        // Either no api_surface at all, or one without constants.js.
        if let Some(start) = d.digest_text.find("<api_surface>") {
            let end = d.digest_text[start..].find("</api_surface>").unwrap();
            let preamble = &d.digest_text[start..start + end];
            assert!(
                !preamble.contains("constants.js"),
                "preamble must skip files with no symbols: {preamble}"
            );
        }
    }

    /// One-shot live-network dumper. Fetches sindresorhus/got from
    /// codeload.github.com, runs the digester, writes the rendered
    /// `digest_text` to a fixed file path on disk. Marked `#[ignore]`
    /// so it never runs under `cargo test`; invoke explicitly via
    /// `cargo test --lib dump_got_digest_to_file -- --ignored --nocapture`.
    /// Used by the Phase 1 verify step of the final tuning pass.
    #[tokio::test]
    #[ignore]
    async fn dump_got_digest_to_file() {
        use std::env;
        let parent = env::temp_dir().join("pp-got-digest-dump");
        if parent.exists() {
            let _ = std::fs::remove_dir_all(&parent);
        }
        let extracted = super::fetch_github_tarball(
            "sindresorhus",
            "got",
            "main",
            &parent,
        )
        .await
        .expect("fetch tarball");
        let source = DigestSource::Github {
            owner: "sindresorhus".to_string(),
            repo: "got".to_string(),
            branch: "main".to_string(),
            html_url: "https://github.com/sindresorhus/got".to_string(),
        };
        let d = digest_directory(&extracted, source, &DigestConfig::default())
            .expect("digest");
        let out_path = env::temp_dir().join("got_digest.txt");
        std::fs::write(&out_path, &d.digest_text).expect("write");
        eprintln!(
            "[dump] wrote {} bytes to {} (files={} elided={} tokens={})",
            d.digest_text.len(),
            out_path.display(),
            d.file_count,
            d.elided_count,
            d.token_count_estimate
        );
    }

    #[test]
    fn api_surface_renders_with_file_paths_and_symbols() {
        // Spec-aligned test (the tuning-pass step 1.3 test). Confirms
        // the new markdown-heading + bullet format produces visible
        // file paths AND the instructional footer.
        let td = tmpdir();
        write(
            &td.path().join("source").join("core").join("options.ts"),
            br#"export class Options { }
export function getDefaults() { return {}; }
export type OptionsInit = Partial<Options>;
"#,
        );
        write(
            &td.path().join("source").join("core").join("errors.ts"),
            br#"export class HTTPError extends Error { }
export class RequestError extends Error { }
"#,
        );
        write(&td.path().join("package.json"), br#"{"name":"x"}"#);
        let d = digest_directory(
            td.path(),
            DigestSource::Local {
                path: td.path().to_string_lossy().to_string(),
            },
            &DigestConfig::default(),
        )
        .unwrap();
        let surface_start = d
            .digest_text
            .find("<api_surface>")
            .expect("api_surface missing");
        let surface_end = d.digest_text[surface_start..]
            .find("</api_surface>")
            .expect("api_surface unterminated")
            + surface_start;
        let surface = &d.digest_text[surface_start..surface_end];

        // Must contain file paths verbatim.
        assert!(
            surface.contains("source/core/options.ts"),
            "missing options.ts path in: {surface}"
        );
        assert!(
            surface.contains("source/core/errors.ts"),
            "missing errors.ts path in: {surface}"
        );

        // Must contain symbol names.
        assert!(surface.contains("Options"));
        assert!(surface.contains("getDefaults"));
        assert!(surface.contains("HTTPError"));
        assert!(surface.contains("RequestError"));

        // Must contain the instructional footer — this is what
        // turns the index from a passive reference into a directive
        // the LLM acts on.
        assert!(
            surface.contains("How to use this index")
                || surface.contains("use these paths verbatim"),
            "footer missing — preamble would be merely informative: {surface}"
        );

        // Markdown structure must be present (### headings + bullet
        // list) — visual prominence is what bumps prompts to L4/L5.
        assert!(
            surface.contains("### source/core/options.ts"),
            "missing markdown heading for options.ts: {surface}"
        );
        assert!(
            surface.contains("- class Options") || surface.contains("- class HTTPError"),
            "missing bullet list under heading: {surface}"
        );
    }

    #[test]
    fn api_surface_caps_total_size_under_budget() {
        // Generate a file with way more than API_PREAMBLE_MAX_ENTRIES
        // exports. The rendered preamble must still respect the byte
        // cap.
        let td = tmpdir();
        let mut huge = String::new();
        for i in 0..200 {
            huge.push_str(&format!("export function fn_{i:04}() {{ return {i}; }}\n"));
        }
        write(&td.path().join("src").join("big.js"), huge.as_bytes());
        write(&td.path().join("package.json"), br#"{"name":"x"}"#);
        let d = digest_directory(
            td.path(),
            DigestSource::Local { path: td.path().to_string_lossy().to_string() },
            &DigestConfig::default(),
        )
        .unwrap();
        // Walk the preamble bounds.
        let start = d.digest_text.find("<api_surface>").unwrap();
        let end_marker = "</api_surface>";
        let end = d.digest_text[start..].find(end_marker).unwrap() + end_marker.len();
        let preamble = &d.digest_text[start..start + end];
        assert!(
            preamble.len() <= API_PREAMBLE_MAX_BYTES + 256,
            "preamble overflowed: {} > {}",
            preamble.len(),
            API_PREAMBLE_MAX_BYTES
        );
    }

    #[test]
    fn digest_skips_node_modules() {
        let td = tmpdir();
        write(&td.path().join("README.md"), b"root");
        // node_modules/ must never recurse, even without .gitignore.
        write(
            &td.path().join("node_modules").join("foo").join("index.js"),
            b"module.exports = 1;",
        );
        let d = digest_directory(
            td.path(),
            DigestSource::Local { path: td.path().to_string_lossy().to_string() },
            &DigestConfig::default(),
        )
        .unwrap();
        assert!(!d.digest_text.contains("node_modules"));
        assert!(d.digest_text.contains("README.md"));
    }
}

