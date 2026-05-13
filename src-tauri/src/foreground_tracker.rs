//! Tracks the most recent non-PromptForge foreground window on Windows
//! so the floating capsule's Enhance button can restore that window's
//! focus before synthesising Ctrl+C. The global hotkey path doesn't need
//! this because it captures while the user is still focused in their
//! target app — clicking the capsule, by contrast, steals focus to our
//! own webview.
//!
//! Implemented as a background polling thread (200ms cadence). Light
//! and resilient — no event hooks to clean up, survives focus changes
//! that happen between polls because we only care about the LATEST
//! user window, not the full history.
//!
//! Windows-only for now. The non-Windows stubs let the module compile
//! on macOS/Linux without `cfg` clutter at every call site.

#[cfg(target_os = "windows")]
use std::sync::atomic::{AtomicIsize, Ordering};

#[cfg(target_os = "windows")]
static LAST_USER_HWND: AtomicIsize = AtomicIsize::new(0);

const POLL_INTERVAL_MS: u64 = 200;

/// Delay after `SetForegroundWindow` before we synthesise the Ctrl+C
/// capture. Windows needs a beat for activation, paint, and the target
/// app's input loop to start receiving input. Empirically 80ms is the
/// minimum that consistently lands on the right window.
pub const FOCUS_RESTORE_SETTLE_MS: u64 = 80;

#[cfg(target_os = "windows")]
pub fn spawn() {
    use std::thread;
    use std::time::Duration;
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{
        GetForegroundWindow, GetWindowThreadProcessId,
    };

    let our_pid = std::process::id();
    thread::spawn(move || loop {
        unsafe {
            let hwnd: HWND = GetForegroundWindow();
            if !hwnd.is_invalid() {
                let mut pid: u32 = 0;
                let _ = GetWindowThreadProcessId(hwnd, Some(&mut pid as *mut u32));
                if pid != 0 && pid != our_pid {
                    LAST_USER_HWND.store(hwnd.0 as isize, Ordering::Relaxed);
                }
            }
        }
        thread::sleep(Duration::from_millis(POLL_INTERVAL_MS));
    });
    println!("[foreground-tracker] started (poll every {POLL_INTERVAL_MS}ms)");
}

#[cfg(not(target_os = "windows"))]
pub fn spawn() {
    // No-op on non-Windows builds. The Enhance button still works through
    // the existing capture pipeline; only the focus-restore step is
    // skipped, which on those platforms is rarely needed because
    // click-to-focus semantics differ.
}

/// If the current foreground window belongs to our own process, restore
/// the most recently tracked user window to foreground. Returns `true`
/// if the foreground is already a user window (no action needed) OR if
/// the restoration call succeeded.
#[cfg(target_os = "windows")]
pub fn restore_user_foreground_if_needed() -> bool {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{
        GetForegroundWindow, GetWindowThreadProcessId, SetForegroundWindow,
    };

    let our_pid = std::process::id();
    unsafe {
        let current: HWND = GetForegroundWindow();
        if current.is_invalid() {
            return false;
        }
        let mut current_pid: u32 = 0;
        let _ = GetWindowThreadProcessId(current, Some(&mut current_pid as *mut u32));
        if current_pid != our_pid {
            // User app is already foreground — nothing to do.
            return true;
        }

        let saved = LAST_USER_HWND.load(Ordering::Relaxed);
        if saved == 0 {
            println!("[foreground-tracker] no saved user window to restore");
            return false;
        }
        let target = HWND(saved as *mut _);
        let ok = SetForegroundWindow(target).as_bool();
        if !ok {
            println!("[foreground-tracker] SetForegroundWindow refused (HWND {saved:#x})");
        }
        ok
    }
}

#[cfg(not(target_os = "windows"))]
pub fn restore_user_foreground_if_needed() -> bool {
    // See comment in spawn() — not needed on non-Windows.
    true
}
