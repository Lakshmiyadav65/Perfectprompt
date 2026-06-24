use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Result};
use tauri::{AppHandle, Runtime};
use tauri_plugin_clipboard_manager::ClipboardExt;

#[cfg(not(target_os = "windows"))]
use enigo::{Direction, Enigo, Key, Keyboard, Settings};

// Non-Windows fallback when we can't poll for modifier release.
#[cfg(not(target_os = "windows"))]
const KEY_RELEASE_SETTLE_MS: u64 = 250;
#[cfg(target_os = "windows")]
const KEY_RELEASE_MAX_WAIT_MS: u64 = 1500;
#[cfg(target_os = "windows")]
const KEY_RELEASE_POLL_MS: u64 = 20;
const CLIPBOARD_SETTLE_MS: u64 = 120;

fn snippet(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= 40 {
        format!("{:?}", s)
    } else {
        let head: String = chars.iter().take(40).collect();
        format!("{:?}…({} chars)", head, chars.len())
    }
}

pub async fn capture_selection<R: Runtime>(app: &AppHandle<R>) -> Result<String> {
    let clipboard = app.clipboard();

    let original = clipboard.read_text().ok();
    println!(
        "[capture] step 1/6 read-original: {}",
        match &original {
            Some(s) => snippet(s),
            None => "(empty/non-text)".to_string(),
        }
    );

    let sentinel = make_sentinel();
    clipboard
        .write_text(sentinel.clone())
        .map_err(|e| anyhow!("could not plant sentinel on clipboard: {e}"))?;
    println!("[capture] step 2/6 sentinel-written ({} chars)", sentinel.len());

    #[cfg(target_os = "windows")]
    {
        wait_for_modifier_release().await;
        diag_foreground_window();
        diag_modifier_state();
    }
    #[cfg(not(target_os = "windows"))]
    {
        println!("[capture] step 3/6 waiting {KEY_RELEASE_SETTLE_MS}ms for hotkey modifiers to release");
        tokio::time::sleep(Duration::from_millis(KEY_RELEASE_SETTLE_MS)).await;
    }

    println!("[capture] step 4/6 dispatching synthetic Ctrl+C");
    send_copy()?;

    println!("[capture] step 5/6 waiting {CLIPBOARD_SETTLE_MS}ms for clipboard to settle");
    tokio::time::sleep(Duration::from_millis(CLIPBOARD_SETTLE_MS)).await;

    let after = clipboard.read_text().ok();
    println!(
        "[capture] step 6/6 read-after: {}",
        match &after {
            Some(s) => snippet(s),
            None => "(empty/non-text)".to_string(),
        }
    );

    // Restore the clipboard. If there was no readable original, write an empty
    // string so we don't leave our sentinel behind for the next press.
    let _ = clipboard.write_text(original.unwrap_or_default());

    match after {
        Some(text) if text == sentinel => Err(anyhow!(
            "synthetic Ctrl+C produced no copy — sentinel survived. Either no text was selected, focus shifted, or the active app blocks synthetic input"
        )),
        Some(text) => Ok(text),
        None => Err(anyhow!(
            "clipboard was unreadable after Ctrl+C — the focused app may block synthetic input"
        )),
    }
}

pub async fn replace_selection<R: Runtime>(app: &AppHandle<R>, new_text: &str) -> Result<()> {
    let clipboard = app.clipboard();
    clipboard
        .write_text(new_text.to_string())
        .map_err(|e| anyhow!("clipboard write failed: {e}"))?;

    tokio::time::sleep(Duration::from_millis(CLIPBOARD_SETTLE_MS)).await;

    send_paste()?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Windows: bypass enigo and call SendInput directly. enigo 0.2's Key::Unicode
// uses WM_CHAR which can't trigger Ctrl+C, and Key::Other(VK_C) didn't help in
// our testing — likely an enigo packaging issue. Going to the OS API directly.
// ---------------------------------------------------------------------------
#[cfg(target_os = "windows")]
fn send_copy() -> Result<()> {
    raw_ctrl_letter(b'C', "capture")
}

#[cfg(target_os = "windows")]
fn send_paste() -> Result<()> {
    raw_ctrl_letter(b'V', "replace")
}

#[cfg(target_os = "windows")]
fn raw_ctrl_letter(letter: u8, log_tag: &str) -> Result<()> {
    use windows::Win32::Foundation::GetLastError;
    use windows::Win32::UI::Input::KeyboardAndMouse::{SendInput, INPUT, VIRTUAL_KEY, VK_CONTROL};

    let vk_letter = VIRTUAL_KEY(letter as u16);
    let inputs = [
        make_keybd(VK_CONTROL, false),
        make_keybd(vk_letter, false),
        make_keybd(vk_letter, true),
        make_keybd(VK_CONTROL, true),
    ];

    let cb = std::mem::size_of::<INPUT>() as i32;
    let sent = unsafe { SendInput(&inputs, cb) };
    if sent as usize != inputs.len() {
        let last = unsafe { GetLastError() };
        return Err(anyhow!(
            "SendInput delivered {sent}/{} events, GetLastError={:?}",
            inputs.len(),
            last
        ));
    }
    println!("[{log_tag}]   SendInput delivered {} events successfully", sent);
    Ok(())
}

#[cfg(target_os = "windows")]
fn make_keybd(
    vk: windows::Win32::UI::Input::KeyboardAndMouse::VIRTUAL_KEY,
    up: bool,
) -> windows::Win32::UI::Input::KeyboardAndMouse::INPUT {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP,
    };
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: 0,
                dwFlags: if up { KEYEVENTF_KEYUP } else { KEYBD_EVENT_FLAGS(0) },
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

#[cfg(target_os = "windows")]
async fn wait_for_modifier_release() {
    use std::time::Instant;
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_CONTROL, VK_MENU};

    let start = Instant::now();
    let max = Duration::from_millis(KEY_RELEASE_MAX_WAIT_MS);
    let poll = Duration::from_millis(KEY_RELEASE_POLL_MS);

    loop {
        let held = unsafe {
            let ctrl = (GetAsyncKeyState(VK_CONTROL.0 as i32) as u16 & 0x8000) != 0;
            let alt = (GetAsyncKeyState(VK_MENU.0 as i32) as u16 & 0x8000) != 0;
            ctrl || alt
        };
        if !held {
            println!(
                "[capture] step 3/6 modifiers released after {}ms",
                start.elapsed().as_millis()
            );
            return;
        }
        if start.elapsed() >= max {
            println!(
                "[capture] step 3/6 modifiers STILL HELD after {}ms — proceeding anyway",
                max.as_millis()
            );
            return;
        }
        tokio::time::sleep(poll).await;
    }
}

#[cfg(target_os = "windows")]
fn diag_modifier_state() {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        GetAsyncKeyState, VK_CONTROL, VK_LWIN, VK_MENU, VK_SHIFT,
    };
    unsafe {
        let held = |vk: windows::Win32::UI::Input::KeyboardAndMouse::VIRTUAL_KEY| -> bool {
            (GetAsyncKeyState(vk.0 as i32) as u16 & 0x8000) != 0
        };
        let ctrl = held(VK_CONTROL);
        let alt = held(VK_MENU);
        let shift = held(VK_SHIFT);
        let win = held(VK_LWIN);
        println!(
            "[capture]   modifier state: ctrl={ctrl} alt={alt} shift={shift} win={win}"
        );
    }
}

#[cfg(target_os = "windows")]
fn diag_foreground_window() {
    use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowTextW};
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.is_invalid() {
            println!("[capture]   foreground window: <none/invalid>");
            return;
        }
        let mut buf = [0u16; 256];
        let len = GetWindowTextW(hwnd, &mut buf);
        let title = if len > 0 {
            String::from_utf16_lossy(&buf[..len as usize])
        } else {
            "<no title>".to_string()
        };
        println!("[capture]   foreground window: HWND={:?} title={title:?}", hwnd.0);
    }
}

// ---------------------------------------------------------------------------
// Non-Windows: use enigo as before.
// ---------------------------------------------------------------------------
#[cfg(not(target_os = "windows"))]
fn send_copy() -> Result<()> {
    send_modifier_combo('c')
}

#[cfg(not(target_os = "windows"))]
fn send_paste() -> Result<()> {
    send_modifier_combo('v')
}

#[cfg(not(target_os = "windows"))]
fn send_modifier_combo(letter: char) -> Result<()> {
    let mut enigo = Enigo::new(&Settings::default())
        .map_err(|e| anyhow!("enigo init failed: {e:?}"))?;

    let modifier = if cfg!(target_os = "macos") {
        Key::Meta
    } else {
        Key::Control
    };

    enigo
        .key(modifier, Direction::Press)
        .map_err(|e| anyhow!("modifier press failed: {e:?}"))?;
    enigo
        .key(Key::Unicode(letter), Direction::Click)
        .map_err(|e| anyhow!("'{letter}' click failed: {e:?}"))?;
    enigo
        .key(modifier, Direction::Release)
        .map_err(|e| anyhow!("modifier release failed: {e:?}"))?;

    Ok(())
}

fn make_sentinel() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("__PROMPTFORGE_CAPTURE_SENTINEL_{nanos}__")
}
