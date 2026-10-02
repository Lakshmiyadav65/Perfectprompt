//! Hold Ctrl+Alt to talk: voice-enhance push-to-talk on a modifier-only chord.
//!
//! A modifier-only chord can't be a global shortcut — `RegisterHotKey` needs a
//! non-modifier key — so, like `space_ptt`, we **poll** `GetAsyncKeyState` on
//! a background thread instead of hooking the keyboard. Nothing is swallowed
//! or re-injected, so typing and every Ctrl+Alt+<key> shortcut (including the
//! enhance hotkey) behave exactly as before.
//!
//!   * Hold Ctrl + Left Alt, with nothing else, past [`HOLD_THRESHOLD_MS`] →
//!     start a voice-enhance capture ([`crate::mic::begin`]).
//!   * Release either key → end it ([`crate::mic::end`]).
//!   * Any other key or mouse button during the gesture (Ctrl+Alt+E,
//!     Ctrl+Alt+click…) disqualifies it until both keys are released; a quick
//!     Ctrl+Alt+<key> never reaches the threshold, so it never records.
//!
//! Only **Left** Alt arms it: AltGr — which Windows reports as Left Ctrl +
//! Right Alt — is how many keyboard layouts type @, €, { and so on.

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

    /// How long Ctrl+Alt must be held before a capture starts. Long enough
    /// that reaching for the third key of a Ctrl+Alt shortcut never records.
    const HOLD_THRESHOLD_MS: u128 = 300;
    /// Poll cadence. Fast enough that a key pressed mid-gesture is seen.
    const POLL_MS: u64 = 15;
    /// Hard cap on a single capture — a backstop so a missed release (lock /
    /// secure desktop) can never leave the mic hot for more than this.
    const MAX_RECORD_MS: u128 = 120_000;

    static APP: OnceLock<tauri::AppHandle> = OnceLock::new();
    /// Feature gate. `uninstall` clears it; the poll thread sees that, ends any
    /// live capture, and exits.
    static ENABLED: AtomicBool = AtomicBool::new(false);
    /// Whether the poll thread is alive (so `install` is idempotent).
    static RUNNING: AtomicBool = AtomicBool::new(false);
    /// Whether a push-to-talk capture started by this poller is in flight.
    static RECORDING: AtomicBool = AtomicBool::new(false);

    pub fn set_app(app: &tauri::AppHandle) {
        let _ = APP.set(app.clone());
    }

    pub fn install() {
        ENABLED.store(true, Ordering::SeqCst);
        if RUNNING.swap(true, Ordering::SeqCst) {
            return;
        }
        std::thread::spawn(poll_loop);
        println!("[ctrl-alt-ptt] Ctrl+Alt hold-to-talk enabled");
    }

    pub fn uninstall() {
        ENABLED.store(false, Ordering::SeqCst);
    }

    fn poll_loop() {
        // State for the gesture in progress, owned by this thread.
        let mut held_since: Option<Instant> = None;
        let mut recording_since: Option<Instant> = None;
        // Set when another key joins the gesture (or after a max-duration
        // force-stop); cleared only once Ctrl and Alt are both released.
        let mut spoiled = false;

        loop {
            if !ENABLED.load(Ordering::SeqCst) {
                end_if_recording();
                RUNNING.store(false, Ordering::SeqCst);
                println!("[ctrl-alt-ptt] disabled");
                return;
            }

            let ctrl = key_down(VK_LCONTROL) || key_down(VK_RCONTROL);
            let lalt = key_down(VK_LMENU);

            if !ctrl && !lalt {
                // Gesture over — reset and stop any capture.
                held_since = None;
                recording_since = None;
                spoiled = false;
                end_if_recording();
            } else {
                // Only scan the whole keyboard while a gesture is live, so the
                // idle cost stays at three key reads per tick.
                spoiled |= other_input_down();

                if !(ctrl && lalt) || spoiled {
                    // One key let go, or the gesture became a shortcut.
                    held_since = None;
                    recording_since = None;
                    end_if_recording();
                } else {
                    let now = Instant::now();
                    let start = *held_since.get_or_insert(now);
                    match recording_since {
                        None if now.duration_since(start).as_millis() >= HOLD_THRESHOLD_MS => {
                            begin_capture();
                            recording_since = Some(now);
                        }
                        Some(rs) if now.duration_since(rs).as_millis() >= MAX_RECORD_MS => {
                            end_if_recording();
                            recording_since = None;
                            spoiled = true; // require a release before re-arming
                        }
                        _ => {}
                    }
                }
            }

            std::thread::sleep(Duration::from_millis(POLL_MS));
        }
    }

    fn begin_capture() {
        RECORDING.store(true, Ordering::SeqCst);
        if let Some(app) = APP.get() {
            if let Err(e) = crate::mic::begin(app, crate::mic::MicMode::Enhance) {
                eprintln!("[ctrl-alt-ptt] begin failed: {e:#}");
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
