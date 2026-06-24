//! App-styled in-app toast window. Renders inside the React Toast
//! component (`#/toast` route) and is positioned bottom-right of the
//! primary monitor's work area. Borderless, transparent, always-on-top.
//!
//! Called from [`crate::tray::notify_fallback`] to replace the
//! OS-native rate-limit notification with a dark-UI / orange-accent
//! toast that matches the rest of PerfectPrompt.
//!
//! The lifecycle is asymmetric: this module owns SHOWING the window
//! (position + show); the React component owns HIDING (via the Tauri
//! `core:window:allow-hide` permission it already has, called from
//! the toast's auto-dismiss timer or close button). That keeps the
//! Rust side dumb — it doesn't need to know the dismiss duration.

use anyhow::{anyhow, Result};
use tauri::{AppHandle, LogicalSize, Manager, PhysicalPosition, Runtime};

const TOAST_LABEL: &str = "toast";

/// Window size in LOGICAL pixels — keep in lock-step with
/// `tauri.conf.json`. The display-side arithmetic below converts
/// these to physical pixels via the window's DPI scale factor so
/// the toast positions correctly on 125% / 150% / 200% scaled
/// displays (without this conversion the toast clips off the right
/// edge on any non-100% scaling).
const WIN_W: i32 = 360;
const WIN_H: i32 = 96;

/// Margins from the bottom-right corner of the work area, in
/// LOGICAL pixels. Tuned for a balanced placement — close enough to
/// the right edge that the gap doesn't read as accidental, but not
/// flush against the scrollbar / screen edge. 32px bottom keeps the
/// toast clear of the Windows taskbar.
const EDGE_MARGIN_RIGHT: i32 = 24;
const EDGE_MARGIN_BOTTOM: i32 = 32;

/// Position the toast window at the bottom-right of the primary
/// monitor's work area and show it. The React Toast component will
/// pick up the [`crate::tray`]-emitted `pipeline:fallback` event and
/// render the message; it owns the auto-dismiss + hide back to this
/// window.
///
/// Best-effort. Failures are logged (caller's `eprintln!`) but never
/// propagated — a failed toast must not block the pipeline.
pub fn show<R: Runtime>(app: &AppHandle<R>) -> Result<()> {
    let window = app
        .get_webview_window(TOAST_LABEL)
        .ok_or_else(|| anyhow!("toast window not found"))?;

    // GetMonitorInfoW returns physical pixels; we need to subtract
    // physical dimensions to land at the right physical position.
    // Without this scale step the toast is offset by `(scale-1) ×
    // (WIN_W + EDGE_MARGIN)` to the right at non-100% DPI — at 125%
    // that's enough to push the close button off-screen.
    let scale = window.scale_factor().unwrap_or(1.0);
    let win_w_phys = (WIN_W as f64 * scale).round() as i32;
    let win_h_phys = (WIN_H as f64 * scale).round() as i32;
    let margin_right_phys = (EDGE_MARGIN_RIGHT as f64 * scale).round() as i32;
    let margin_bottom_phys = (EDGE_MARGIN_BOTTOM as f64 * scale).round() as i32;

    let (x_min, y_min, x_max, y_max) = primary_work_area();
    let x = (x_max - win_w_phys - margin_right_phys).max(x_min);
    let y = (y_max - win_h_phys - margin_bottom_phys).max(y_min);

    window
        .set_size(LogicalSize::new(WIN_W as f64, WIN_H as f64))
        .map_err(|e| anyhow!("set_size failed: {e}"))?;
    window
        .set_position(PhysicalPosition::new(x, y))
        .map_err(|e| anyhow!("set_position failed: {e}"))?;
    window
        .show()
        .map_err(|e| anyhow!("show failed: {e}"))?;
    Ok(())
}

#[cfg(target_os = "windows")]
fn primary_work_area() -> (i32, i32, i32, i32) {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTOPRIMARY,
    };
    unsafe {
        // Anchor to a guaranteed-on-primary-monitor point. The point
        // (0,0) might not land on the primary on multi-monitor setups
        // where the primary isn't top-left, so we use the documented
        // MONITOR_DEFAULTTOPRIMARY flag instead.
        let pt = POINT { x: 0, y: 0 };
        let monitor = MonitorFromPoint(pt, MONITOR_DEFAULTTOPRIMARY);
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if GetMonitorInfoW(monitor, &mut info).as_bool() {
            (
                info.rcWork.left,
                info.rcWork.top,
                info.rcWork.right,
                info.rcWork.bottom,
            )
        } else {
            (0, 0, 1920, 1040) // fallback approximating a 1080p screen minus taskbar
        }
    }
}

#[cfg(not(target_os = "windows"))]
fn primary_work_area() -> (i32, i32, i32, i32) {
    (0, 0, 1920, 1040)
}
