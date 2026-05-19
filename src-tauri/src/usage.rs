//! Local daily-usage counter for BYOK enhancements.
//!
//! Rust is the canonical source of truth because the success point of
//! every enhancement runs in `pipeline::run` — regardless of which UI
//! surface (silent hotkey from VS Code/IDEs, clarify popup, question
//! card, main app) triggered it. The previous setup put the counter on
//! the frontend behind a Tauri event listener, which depended on at
//! least one webview being awake to receive the event. Background
//! webviews (the main window when the user is focused in VS Code) don't
//! always run their listeners promptly, so the counter would silently
//! miss hotkey-from-IDE enhancements.
//!
//! Hosted-tier users still see the server-authoritative quota — this
//! counter ticks for them too, but the UI prefers `hosted.used` when
//! present (see useEnhancementUsage). The limit-reached check in
//! `pipeline::run` only fires on the BYOK path; the hosted path lets
//! Supabase's `consume_quota` enforce limits.
//!
//! Persistence: `usage.json` in the app config dir, alongside
//! `settings.json`. Resets when the local date rolls over.

use std::path::PathBuf;
use std::sync::Mutex;

use anyhow::{Context, Result};
use chrono::Local;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, Runtime, State};

const USAGE_FILE: &str = "usage.json";

/// BYOK daily ceiling. Matches the frontend's `DAILY_LIMIT` constant so
/// the sidebar's bar fills proportionally to what the pipeline enforces.
pub const DAILY_LIMIT: u32 = 50;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct StoredUsage {
    /// YYYY-MM-DD in the user's local timezone.
    date: String,
    count: u32,
}

/// In-memory state guarded by a mutex so the increment-and-emit cycle
/// is atomic. The disk file is the persistence layer; this is the
/// hot-path cache.
pub struct UsageState {
    inner: Mutex<StoredUsage>,
}

impl UsageState {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(StoredUsage::default()),
        }
    }
}

/// Public payload emitted on `usage:changed` and returned from
/// `get_usage_state`. Mirrors the localStorage shape the hook already
/// understands.
#[derive(Debug, Clone, Serialize)]
pub struct UsageSnapshot {
    pub date: String,
    pub used: u32,
    pub limit: u32,
    pub remaining: u32,
    pub limit_reached: bool,
}

fn today_local() -> String {
    Local::now().format("%Y-%m-%d").to_string()
}

fn usage_path<R: Runtime>(app: &AppHandle<R>) -> Result<PathBuf> {
    let dir = app
        .path()
        .app_config_dir()
        .context("could not resolve app config dir")?;
    std::fs::create_dir_all(&dir).context("could not create app config dir")?;
    Ok(dir.join(USAGE_FILE))
}

fn read_from_disk<R: Runtime>(app: &AppHandle<R>) -> StoredUsage {
    let Ok(path) = usage_path(app) else {
        return StoredUsage::default();
    };
    match std::fs::read_to_string(&path) {
        Ok(s) => serde_json::from_str::<StoredUsage>(&s).unwrap_or_default(),
        Err(_) => StoredUsage::default(),
    }
}

fn write_to_disk<R: Runtime>(app: &AppHandle<R>, stored: &StoredUsage) -> Result<()> {
    let path = usage_path(app)?;
    let json = serde_json::to_string_pretty(stored).context("serialize usage")?;
    std::fs::write(&path, json).context("write usage.json")?;
    Ok(())
}

/// Reconcile the in-memory state with the date: if the stored date is
/// stale, reset to today's zero. Called inside every public function so
/// the daily reset is automatic without a separate cron-style task.
fn reconcile(stored: &mut StoredUsage) {
    let today = today_local();
    if stored.date != today {
        stored.date = today;
        stored.count = 0;
    }
}

fn snapshot_from(stored: &StoredUsage) -> UsageSnapshot {
    let used = stored.count.min(DAILY_LIMIT);
    UsageSnapshot {
        date: stored.date.clone(),
        used,
        limit: DAILY_LIMIT,
        remaining: DAILY_LIMIT.saturating_sub(used),
        limit_reached: used >= DAILY_LIMIT,
    }
}

/// Lazily hydrate the in-memory state from disk on first read. Mutex
/// caller owns the guard so the hydrate-then-reconcile is atomic.
fn ensure_loaded<R: Runtime>(app: &AppHandle<R>, stored: &mut StoredUsage) {
    if stored.date.is_empty() {
        *stored = read_from_disk(app);
    }
    reconcile(stored);
}

/// Snapshot of the current count for the frontend. Hydrates from disk
/// on first call per process; subsequent calls hit memory.
pub fn snapshot<R: Runtime>(app: &AppHandle<R>) -> UsageSnapshot {
    let state = app.state::<UsageState>();
    let mut stored = state.inner.lock().expect("usage mutex poisoned");
    ensure_loaded(app, &mut stored);
    snapshot_from(&stored)
}

/// True when the user has reached the daily ceiling. The pipeline
/// consults this before kicking off Stage D on the BYOK path.
pub fn limit_reached<R: Runtime>(app: &AppHandle<R>) -> bool {
    snapshot(app).limit_reached
}

/// Increment the counter by one and broadcast the new snapshot. Called
/// from `pipeline::run`'s single success point so every entry path
/// (hotkey-from-IDE, clarify popup, question card, main app) ticks the
/// same counter exactly once per enhancement.
///
/// Persistence failures are logged but non-fatal — the in-memory count
/// stays correct and the next successful write reconciles disk.
pub fn increment<R: Runtime>(app: &AppHandle<R>) -> UsageSnapshot {
    let state = app.state::<UsageState>();
    let snap = {
        let mut stored = state.inner.lock().expect("usage mutex poisoned");
        ensure_loaded(app, &mut stored);
        // Saturating add so we never overflow past the ceiling. The
        // frontend treats >= DAILY_LIMIT as "limit reached" identically
        // to == DAILY_LIMIT.
        stored.count = stored.count.saturating_add(1).min(DAILY_LIMIT);
        let snap = snapshot_from(&stored);
        if let Err(e) = write_to_disk(app, &stored) {
            eprintln!("[usage] write_to_disk failed: {e:#}");
        }
        snap
    };

    if let Err(e) = app.emit("usage:changed", &snap) {
        eprintln!("[usage] emit usage:changed failed: {e}");
    }
    snap
}

/// Tauri command — frontend calls this on mount to seed its display
/// state before any `usage:changed` events arrive. Idempotent.
#[tauri::command]
pub fn get_usage_state<R: Runtime>(
    app: AppHandle<R>,
    _state: State<'_, UsageState>,
) -> UsageSnapshot {
    snapshot(&app)
}
