//! Hold-to-talk push-to-talk on a non-typing key (Right Ctrl).
//!
//! (Module keeps the historical name `space_ptt`; the trigger moved from the
//! spacebar to **Right Ctrl** after an adversarial review showed a bare-Space
//! low-level hook has an unavoidable cost — it must swallow and re-emit every
//! space, which reorders fast typing and opens a dozen race/lifecycle edges.)
//!
//! Right Ctrl is not a key you type into text, so we don't intercept it at
//! all — no `WH_KEYBOARD_LL` hook, no key consumption, no synthesized events.
//! We simply **poll** `GetAsyncKeyState(VK_RCONTROL)` on a background thread:
//!
//!   * Hold Right Ctrl past [`HOLD_THRESHOLD_MS`] → start a voice-enhance
//!     capture ([`crate::mic::begin`]).
//!   * Release → end it ([`crate::mic::end`]).
//!   * A chord (Right Ctrl + another modifier) is ignored, so Ctrl shortcuts
//!     still work.
//!
//! This design sidesteps the whole class of hook bugs the review found:
//!   * No hook to leak / race on install/uninstall (just a thread + an atomic
//!     `ENABLED` flag it checks each tick).
//!   * Nothing is swallowed or re-injected, so typing is never touched.
//!   * Polling reads the *real* key state, so a release missed during a lock /
//!     UAC / secure-desktop is caught on return (Right Ctrl reads up → end),
//!     and [`MAX_RECORD_MS`] is a hard backstop against a stuck-hot mic.

#[cfg(target_os = "windows")]
pub use imp::{install, set_app, uninstall};

#[cfg(not(target_os = "windows"))]
pub fn set_app(_app: &tauri::AppHandle) {}
#[cfg(not(target_os = "windows"))]
pub fn install() {}
#[cfg(not(target_os = "windows"))]
pub fn uninstall() {}

#[cfg(target_os = "windows")]
mod imp {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::OnceLock;
    use std::time::{Duration, Instant};

    use windows::Win32::UI::Input::KeyboardAndMouse::{
        GetAsyncKeyState, VK_LCONTROL, VK_LWIN, VK_MENU, VK_RCONTROL, VK_RWIN, VK_SHIFT,
    };

    /// How long Right Ctrl must be held before a capture starts. Long enough
    /// that a quick Ctrl-shortcut tap never triggers it.
    const HOLD_THRESHOLD_MS: u128 = 200;
    /// Poll cadence. 25ms → press/release detected within a frame, negligible CPU.
    const POLL_MS: u64 = 25;
    /// Hard cap on a single capture — a backstop so a missed release (lock /
    /// secure desktop) can never leave the mic hot for more than this.
    const MAX_RECORD_MS: u128 = 120_000;

    static APP: OnceLock<tauri::AppHandle> = OnceLock::new();
    /// Feature gate. `uninstall` clears it; the poll thread sees that, ends any
    /// live capture, and exits. `install` sets it and starts the thread.
    static ENABLED: AtomicBool = AtomicBool::new(false);
    /// Whether the poll thread is alive (so `install` is idempotent).
    static RUNNING: AtomicBool = AtomicBool::new(false);
    /// Whether a push-to-talk capture is currently in flight.
    static RECORDING: AtomicBool = AtomicBool::new(false);

    pub fn set_app(app: &tauri::AppHandle) {
        let _ = APP.set(app.clone());
    }

    pub fn install() {
        ENABLED.store(true, Ordering::SeqCst);
        // Idempotent: only one poll thread ever runs. If it's already alive it
        // will observe ENABLED=true and resume triggering.
        if RUNNING.swap(true, Ordering::SeqCst) {
            return;
        }
        std::thread::spawn(poll_loop);
        println!("[push-to-talk] Right Ctrl hold-to-talk enabled");
    }

    pub fn uninstall() {
        // The poll thread does the actual teardown (end any live capture, exit)
        // on its next tick — no hook to remove, no cross-thread races.
        ENABLED.store(false, Ordering::SeqCst);
    }

    fn poll_loop() {
        // Local state, owned entirely by this one thread.
        let mut held_since: Option<Instant> = None;
        let mut recording_since: Option<Instant> = None;
        // After a max-duration force-stop we wait for a real release before we
        // allow another capture, so a stuck key can't machine-gun recordings.
        let mut wait_for_release = false;

        loop {
            if !ENABLED.load(Ordering::SeqCst) {
                end_if_recording();
                RUNNING.store(false, Ordering::SeqCst);
                println!("[push-to-talk] disabled");
                return;
            }

            let triggering = rctrl_down() && !other_modifier_down();

            if triggering {
                let now = Instant::now();
                let start = *held_since.get_or_insert(now);

                if wait_for_release {
                    // Still held after a force-stop — do nothing until released.
                } else if !RECORDING.load(Ordering::SeqCst) {
                    if now.duration_since(start).as_millis() >= HOLD_THRESHOLD_MS {
                        begin_capture();
                        recording_since = Some(now);
                    }
                } else if let Some(rs) = recording_since {
                    if now.duration_since(rs).as_millis() >= MAX_RECORD_MS {
                        end_if_recording();
                        recording_since = None;
                        wait_for_release = true; // require release before re-arming
                    }
                }
            } else {
                // Released (or a chord is held) — end any capture and reset.
                held_since = None;
                recording_since = None;
                wait_for_release = false;
                end_if_recording();
            }

            std::thread::sleep(Duration::from_millis(POLL_MS));
        }
    }

    fn begin_capture() {
        RECORDING.store(true, Ordering::SeqCst);
        if let Some(app) = APP.get() {
            if let Err(e) = crate::mic::begin(app, crate::mic::MicMode::Enhance) {
                eprintln!("[push-to-talk] begin failed: {e:#}");
                RECORDING.store(false, Ordering::SeqCst);
            }
        }
    }

    fn end_if_recording() {
        if RECORDING.swap(false, Ordering::SeqCst) {
            if let Some(app) = APP.get() {
                crate::mic::end(app);
            }
        }
    }

    fn key_down(vk: windows::Win32::UI::Input::KeyboardAndMouse::VIRTUAL_KEY) -> bool {
        // High bit of GetAsyncKeyState = currently down.
        (unsafe { GetAsyncKeyState(vk.0 as i32) } as u16 & 0x8000) != 0
    }

    fn rctrl_down() -> bool {
        key_down(VK_RCONTROL)
    }

    /// True if any modifier OTHER than Right Ctrl is held — meaning the user is
    /// doing a keyboard shortcut, not push-to-talk.
    fn other_modifier_down() -> bool {
        key_down(VK_LCONTROL)
            || key_down(VK_SHIFT)
            || key_down(VK_MENU)
            || key_down(VK_LWIN)
            || key_down(VK_RWIN)
    }
}
