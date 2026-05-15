//! Stage F's logging arm — appends one JSONL record per enhancement to
//! today's trace file (`<app_data_dir>/traces/YYYY-MM-DD.jsonl`).
//!
//! Logging failures must never block the pipeline: every I/O error is
//! swallowed with an `eprintln!`. The pipeline calls [`append`] and
//! forgets about it.
//!
//! Schema is stable — adding fields is fine, removing or renaming them
//! breaks any downstream analysis tooling. Tune the pipeline by reading
//! today's JSONL, not by changing the shape here.

use serde::{Deserialize, Serialize};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Manager, Runtime};

/// One trace record per pipeline invocation.
///
/// Optional fields cover the cases where a stage didn't run: a Decline
/// at Stage C never calls the LLM, so `llm_latency_ms` is `None`; a
/// pure cache hit never classifies, so `domain`/`complexity` are `None`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceRecord {
    /// Unix milliseconds when the pipeline started.
    pub ts_ms: u128,
    pub raw_input: String,
    pub input_len: usize,
    /// Route picked at Stage C, or a synthetic value for short-circuits
    /// (e.g. `"intake_too_short"`, `"intake_adversarial"`).
    pub route: String,
    pub domain: Option<String>,
    pub complexity: Option<f32>,
    pub ambiguity: Option<u32>,
    pub cache_hit: bool,
    pub llm_called: bool,
    pub llm_latency_ms: Option<u64>,
    pub raw_llm_output: Option<String>,
    pub final_pasted_output: String,
    /// Names of validator rules that fired during Stage E.
    pub validators_fired: Vec<String>,
    /// `"passed"` | `"repaired"` | `"rejected"` | `"n/a"`.
    pub validation_outcome: String,
    pub reject_reason: Option<String>,
    pub total_latency_ms: u64,
}

/// Append a trace record to today's log file. Best-effort — failures
/// are logged but never propagated.
pub fn append<R: Runtime>(app: &AppHandle<R>, record: &TraceRecord) {
    if let Err(e) = try_append(app, record) {
        eprintln!("[trace] append failed: {e}");
    }
}

fn try_append<R: Runtime>(app: &AppHandle<R>, record: &TraceRecord) -> std::io::Result<()> {
    let path = today_log_path(app)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let line = serde_json::to_string(record)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
    let mut f = OpenOptions::new().create(true).append(true).open(&path)?;
    f.write_all(line.as_bytes())?;
    f.write_all(b"\n")?;
    Ok(())
}

fn today_log_path<R: Runtime>(app: &AppHandle<R>) -> std::io::Result<PathBuf> {
    let dir = app.path().app_data_dir().map_err(|e| {
        std::io::Error::new(
            std::io::ErrorKind::Other,
            format!("resolve app_data_dir: {e}"),
        )
    })?;
    let (y, m, d) = today_ymd();
    Ok(dir
        .join("traces")
        .join(format!("{y:04}-{m:02}-{d:02}.jsonl")))
}

/// Current unix-millis timestamp, for `TraceRecord::ts_ms`. Saturating
/// to 0 on the (impossible-in-practice) pre-epoch case so callers don't
/// have to thread a Result through the orchestrator.
pub fn now_unix_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// UTC date as (year, month, day) — chosen over local time so the log
/// filename is stable regardless of timezone. Users see UTC dates;
/// that's a known trade-off we accept for filename simplicity.
fn today_ymd() -> (i32, u32, u32) {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    civil_from_days(secs.div_euclid(86_400))
}

/// Howard Hinnant's days-from-civil algorithm
/// (http://howardhinnant.github.io/date_algorithms.html, public domain).
/// Converts a Unix-epoch day count to (year, month, day) in the
/// proleptic Gregorian calendar. Pulled in by hand because the brief
/// disallows new dependencies and this is all we need.
fn civil_from_days(days: i64) -> (i32, u32, u32) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn day_zero_is_epoch() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
    }

    #[test]
    fn day_before_epoch_is_1969_dec_31() {
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
    }

    #[test]
    fn y2k_first_day() {
        // 30 years * 365 + 7 leap days = 10957
        assert_eq!(civil_from_days(10_957), (2000, 1, 1));
    }

    #[test]
    fn leap_day_2024() {
        // 1970-01-01 → 2024-02-29: 54 years (13 leap days) + Jan 31 +
        // Feb 28 = 19783 - 1 = 19782 (zero-indexed from epoch)
        assert_eq!(civil_from_days(19_782), (2024, 2, 29));
    }

    #[test]
    fn record_serialises_to_one_jsonl_line() {
        let r = TraceRecord {
            ts_ms: 1_700_000_000_000,
            raw_input: "refactor X".into(),
            input_len: 10,
            route: "code".into(),
            domain: Some("Coding".into()),
            complexity: Some(0.4),
            ambiguity: Some(15),
            cache_hit: false,
            llm_called: true,
            llm_latency_ms: Some(840),
            raw_llm_output: Some("Refactor X to ...".into()),
            final_pasted_output: "Refactor X to ...".into(),
            validators_fired: vec!["strip_preamble".into()],
            validation_outcome: "repaired".into(),
            reject_reason: None,
            total_latency_ms: 860,
        };
        let s = serde_json::to_string(&r).expect("serialise");
        // A JSONL line is one record, no embedded newlines.
        assert!(!s.contains('\n'));
        // Round-trip through serde to catch silent rename mistakes.
        let back: TraceRecord = serde_json::from_str(&s).expect("round-trip");
        assert_eq!(back.route, "code");
        assert_eq!(back.input_len, 10);
        assert!(back.cache_hit == false);
    }
}
