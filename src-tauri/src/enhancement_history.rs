//! User-facing enhancement history.
//!
//! Distinct from `trace.rs`, which logs *everything* the pipeline does
//! (cache hits, declines, fallbacks, validator outcomes) for debugging.
//! This module persists only the user's *successful* enhancements so
//! the Home dashboard can render a clean "Recent Enhancements" list.
//!
//! Storage: append-only JSONL at `<app_data_dir>/enhancement_history.jsonl`,
//! one record per line. The frontend reads via the `list_enhancements`
//! Tauri command (most-recent-first, capped by a `limit` arg) and can
//! drop entries via `delete_enhancement` (rewrites the file without the
//! matching id).

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, Runtime};

const HISTORY_FILE: &str = "enhancement_history.jsonl";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnhancementRecord {
    pub id: String,
    /// ISO-8601 timestamp at append time.
    pub created_at: String,
    /// Original rough input the user selected.
    pub rough: String,
    /// LLM-enhanced output that got pasted back.
    pub enhanced: String,
    /// Routing label: "code" / "writing" / "generic".
    pub route: String,
    pub project_id: Option<String>,
    pub project_name: Option<String>,
}

fn history_path<R: Runtime>(app: &AppHandle<R>) -> Result<PathBuf> {
    let dir = app
        .path()
        .app_data_dir()
        .context("could not resolve app data dir")?;
    std::fs::create_dir_all(&dir).context("could not create app data dir")?;
    Ok(dir.join(HISTORY_FILE))
}

/// ISO-8601 from system time. Falls back to "0" on the (impossible)
/// `SystemTime` before-epoch case so the format never blocks the user.
fn iso_now() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Minimal ISO-8601 in UTC — avoids pulling chrono just for one
    // timestamp string. Frontend renders with toLocaleString anyway.
    format_iso_utc(secs)
}

fn format_iso_utc(secs: u64) -> String {
    // Days since 1970-01-01 (Thursday).
    let days = (secs / 86_400) as i64;
    let secs_of_day = secs % 86_400;
    let hh = (secs_of_day / 3600) as u32;
    let mm = ((secs_of_day % 3600) / 60) as u32;
    let ss = (secs_of_day % 60) as u32;
    let (y, mo, d) = civil_from_days(days);
    format!(
        "{y:04}-{mo:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}Z",
        y = y,
        mo = mo,
        d = d,
        hh = hh,
        mm = mm,
        ss = ss,
    )
}

/// Howard Hinnant's civil_from_days — convert day-count-since-epoch
/// to (year, month, day). Avoids a chrono dependency.
fn civil_from_days(days: i64) -> (i32, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i32 + (era * 400) as i32;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

/// Build a short-but-unique id from the current nanosecond + a slice
/// of the rough input's hash. Strings are sortable lexicographically
/// by timestamp prefix.
fn make_id(rough: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut h: u64 = 0xcbf29ce484222325;
    for b in rough.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{nanos:032x}-{:016x}", h)
}

/// Append a record to the history file and broadcast
/// `enhancement-history:new` so the dashboard can prepend it without
/// polling. Persistence failures are logged but not propagated — a
/// missed history entry is annoying, not fatal.
pub fn append<R: Runtime>(
    app: &AppHandle<R>,
    rough: String,
    enhanced: String,
    route: String,
    project_id: Option<String>,
    project_name: Option<String>,
) {
    if rough.trim().is_empty() || enhanced.trim().is_empty() {
        return;
    }
    if rough.trim() == enhanced.trim() {
        // Unchanged output (e.g., validator repaired-to-original or a
        // model that didn't actually rewrite) isn't worth surfacing.
        return;
    }

    let record = EnhancementRecord {
        id: make_id(&rough),
        created_at: iso_now(),
        rough,
        enhanced,
        route,
        project_id,
        project_name,
    };

    if let Err(e) = write_one(app, &record) {
        eprintln!("[enhancement_history] append failed: {e:#}");
        return;
    }

    if let Err(e) = app.emit("enhancement-history:new", &record) {
        eprintln!("[enhancement_history] emit failed: {e}");
    }
}

fn write_one<R: Runtime>(app: &AppHandle<R>, record: &EnhancementRecord) -> Result<()> {
    let path = history_path(app)?;
    let line = serde_json::to_string(record).context("serialize record")?;
    let mut f = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("open history file: {}", path.display()))?;
    writeln!(f, "{line}").context("write history line")?;
    Ok(())
}

fn read_all<R: Runtime>(app: &AppHandle<R>) -> Result<Vec<EnhancementRecord>> {
    let path = history_path(app)?;
    if !path.exists() {
        return Ok(Vec::new());
    }
    let f = File::open(&path)
        .with_context(|| format!("open history file: {}", path.display()))?;
    let reader = BufReader::new(f);
    let mut out = Vec::new();
    for line in reader.lines() {
        let line = match line {
            Ok(s) => s,
            Err(_) => continue,
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        // Skip malformed lines instead of failing the whole read — a
        // corrupted entry shouldn't take the dashboard down with it.
        if let Ok(record) = serde_json::from_str::<EnhancementRecord>(trimmed) {
            out.push(record);
        }
    }
    Ok(out)
}

// ───────── Tauri commands ─────────

#[tauri::command]
pub fn list_enhancements<R: Runtime>(
    app: AppHandle<R>,
    limit: Option<usize>,
) -> std::result::Result<Vec<EnhancementRecord>, String> {
    let mut all = read_all(&app).map_err(|e| format!("{e:#}"))?;
    // Newest-first. created_at sorts lexicographically because it's
    // a fixed-width ISO-8601 UTC string.
    all.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    if let Some(n) = limit {
        all.truncate(n);
    }
    Ok(all)
}

#[tauri::command]
pub fn delete_enhancement<R: Runtime>(
    app: AppHandle<R>,
    id: String,
) -> std::result::Result<(), String> {
    let all = read_all(&app).map_err(|e| format!("{e:#}"))?;
    let filtered: Vec<EnhancementRecord> =
        all.into_iter().filter(|r| r.id != id).collect();
    let path = history_path(&app).map_err(|e| format!("{e:#}"))?;
    // Rewrite the whole file from the filtered set. O(n) but the
    // dashboard caps at a few thousand entries before we'd want a
    // more clever store.
    let tmp = path.with_extension("jsonl.tmp");
    {
        let mut f = File::create(&tmp).map_err(|e| format!("create tmp: {e}"))?;
        for r in &filtered {
            let line = serde_json::to_string(r).map_err(|e| format!("serialize: {e}"))?;
            writeln!(f, "{line}").map_err(|e| format!("write tmp: {e}"))?;
        }
    }
    std::fs::rename(&tmp, &path).map_err(|e| format!("rename tmp: {e}"))?;
    let _ = app.emit("enhancement-history:deleted", &id);
    Ok(())
}
