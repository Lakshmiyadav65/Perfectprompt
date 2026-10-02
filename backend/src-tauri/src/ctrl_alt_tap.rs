//! Tap Ctrl+Alt (press and release, no other key) to enhance the selection.
//!
//! A modifier-only chord can't be a global shortcut — `RegisterHotKey` needs a
//! non-modifier key — so, like `space_ptt`, we **poll** `GetAsyncKeyState` on
//! a background thread instead of hooking the keyboard. Nothing is swallowed
//! or re-injected, so typing and every Ctrl+Alt+<key> shortcut (including our
//! own enhance hotkey) behave exactly as before.
//!
//! A "tap" is one gesture, from the first of Ctrl / Left Alt going down until
//! both are up again. It fires on release only if:
//!   * both were held together at some point,
//!   * no other key or mouse button was down at any point in the gesture
//!     (so Ctrl+Alt+E, Ctrl+Alt+click, Ctrl+C-then-Alt never count), and
//!   * the whole gesture took at most [`TAP_MAX_MS`] (a long hold isn't a tap).
//!
//! Only **Left** Alt arms it: AltGr — which Windows reports as Left Ctrl +
//! Right Alt — is how many keyboard layouts type @, €, { and so on, and must
//! never trigger an enhancement.

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
        GetAsyncKeyState, VIRTUAL_KEY, VK_CONTROL, VK_LCONTROL, VK_LMENU, VK_MENU, VK_RCONTROL,
    };

    /// Longest press-to-release gesture still treated as a tap.
    const TAP_MAX_MS: u128 = 800;
    /// Poll cadence. Fast enough that a key pressed mid-gesture is seen.
    const POLL_MS: u64 = 15;
    /// Minimum gap between two triggers, so a double tap fires once.
    const COOLDOWN_MS: u128 = 600;

    static APP: OnceLock<tauri::AppHandle> = OnceLock::new();
    /// Feature gate, checked each tick; `uninstall` clears it and the thread exits.
    static ENABLED: AtomicBool = AtomicBool::new(false);
    /// Whether the poll thread is alive (so `install` is idempotent).
    static RUNNING: AtomicBool = AtomicBool::new(false);

    pub fn set_app(app: &tauri::AppHandle) {
        let _ = APP.set(app.clone());
    }

    pub fn install() {
        ENABLED.store(true, Ordering::SeqCst);
        if RUNNING.swap(true, Ordering::SeqCst) {
            return;
        }
        std::thread::spawn(poll_loop);
        println!("[ctrl-alt-tap] enabled");
    }

    pub fn uninstall() {
        ENABLED.store(false, Ordering::SeqCst);
    }

    fn poll_loop() {
        // State for the gesture in progress, owned by this thread.
        let mut gesture_start: Option<Instant> = None;
        let mut both_held = false;
        let mut spoiled = false;
        let mut last_fire: Option<Instant> = None;

        loop {
            if !ENABLED.load(Ordering::SeqCst) {
                RUNNING.store(false, Ordering::SeqCst);
                println!("[ctrl-alt-tap] disabled");
                return;
            }

            let ctrl = key_down(VK_LCONTROL) || key_down(VK_RCONTROL);
            let lalt = key_down(VK_LMENU);

            if ctrl || lalt {
                if gesture_start.is_none() {
                    gesture_start = Some(Instant::now());
                }
                both_held |= ctrl && lalt;
                // Only scan the whole keyboard while a gesture is live, so the
                // idle cost stays at two key reads per tick.
                spoiled |= other_input_down();
            } else if let Some(start) = gesture_start.take() {
                let now = Instant::now();
                let quick = now.duration_since(start).as_millis() <= TAP_MAX_MS;
                let cooled = last_fire
                    .map_or(true, |t| now.duration_since(t).as_millis() >= COOLDOWN_MS);
                if both_held && !spoiled && quick && cooled {
                    last_fire = Some(now);
                    fire();
                }
                both_held = false;
                spoiled = false;
            }

            std::thread::sleep(Duration::from_millis(POLL_MS));
        }
    }

    fn fire() {
        let Some(app) = APP.get() else { return };
        println!("[ctrl-alt-tap] tapped");
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            if let Err(e) = crate::hotkey::run_capture_pipeline(&app, false).await {
                println!("[pipeline] capture failed: {e:#}");
            }
        });
    }

    fn key_down(vk: VIRTUAL_KEY) -> bool {
        // High bit of GetAsyncKeyState = currently down.
        (unsafe { GetAsyncKeyState(vk.0 as i32) } as u16 & 0x8000) != 0
    }

    /// True if any key or mouse button other than Ctrl / Left Alt is down.
    /// Right Alt counts as "other", which is what keeps AltGr out.
    fn other_input_down() -> bool {
        let allowed = [VK_CONTROL, VK_LCONTROL, VK_RCONTROL, VK_MENU, VK_LMENU];
        (0x01u16..=0xFE)
            .map(VIRTUAL_KEY)
            .filter(|vk| !allowed.contains(vk))
            .any(key_down)
    }
}
