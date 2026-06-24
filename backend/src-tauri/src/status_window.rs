use anyhow::{anyhow, Result};
use tauri::{AppHandle, LogicalSize, Manager, PhysicalPosition, Runtime};

const STATUS_LABEL: &str = "status";

// Offset from the cursor so the pill doesn't overlap the cursor itself.
const CURSOR_OFFSET_X: i32 = 18;
const CURSOR_OFFSET_Y: i32 = 18;

// Window size — must match tauri.conf.json (used to clamp away from screen edges).
const WIN_W: i32 = 220;
const WIN_H: i32 = 70;

pub fn show_near_cursor<R: Runtime>(app: &AppHandle<R>) -> Result<()> {
    let window = app
        .get_webview_window(STATUS_LABEL)
        .ok_or_else(|| anyhow!("status window not found"))?;

    let (cx, cy) = cursor_pos();
    let (mx_min, my_min, mx_max, my_max) = work_area_around(cx, cy);

    // Place the pill near the cursor, but clamp so it stays fully on-screen.
    let mut x = cx + CURSOR_OFFSET_X;
    let mut y = cy + CURSOR_OFFSET_Y;
    if x + WIN_W > mx_max {
        x = mx_max - WIN_W - 4;
    }
    if y + WIN_H > my_max {
        y = my_max - WIN_H - 4;
    }
    if x < mx_min {
        x = mx_min + 4;
    }
    if y < my_min {
        y = my_min + 4;
    }

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

pub fn hide<R: Runtime>(app: &AppHandle<R>) -> Result<()> {
    if let Some(window) = app.get_webview_window(STATUS_LABEL) {
        window.hide().map_err(|e| anyhow!("hide failed: {e}"))?;
    }
    Ok(())
}

#[cfg(target_os = "windows")]
fn cursor_pos() -> (i32, i32) {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;
    unsafe {
        let mut p = POINT { x: 0, y: 0 };
        if GetCursorPos(&mut p).is_ok() {
            (p.x, p.y)
        } else {
            (100, 100) // sane fallback
        }
    }
}

#[cfg(target_os = "windows")]
fn work_area_around(cx: i32, cy: i32) -> (i32, i32, i32, i32) {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTONEAREST,
    };
    unsafe {
        let pt = POINT { x: cx, y: cy };
        let monitor = MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST);
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
            (0, 0, 1920, 1080) // fallback
        }
    }
}

#[cfg(not(target_os = "windows"))]
fn cursor_pos() -> (i32, i32) {
    // macOS / Linux cursor positioning will be wired in Phase 7. For now place
    // the pill at a reasonable default (top-right area) so the hide/show plumbing
    // can still be tested.
    (1200, 100)
}

#[cfg(not(target_os = "windows"))]
fn work_area_around(_cx: i32, _cy: i32) -> (i32, i32, i32, i32) {
    (0, 0, 1920, 1080)
}
