use serde::{Deserialize, Serialize};

/// Snapshot of whichever application owned the foreground window at the
/// moment the hotkey fired. The classifier consumes this to decide
/// between the developer-direct path and the guided-questionnaire path
/// (feature: context-aware prompt enhancement).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ActiveAppContext {
    pub process_name: String,
    pub executable_path: String,
    pub window_title: String,
    pub pid: u32,
}

impl ActiveAppContext {
    pub fn is_empty(&self) -> bool {
        self.process_name.is_empty() && self.window_title.is_empty()
    }
}

#[cfg(target_os = "windows")]
pub fn detect_active_app() -> Option<ActiveAppContext> {
    use windows::Win32::Foundation::{CloseHandle, HWND, MAX_PATH};
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_FORMAT,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        GetForegroundWindow, GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId,
    };

    unsafe {
        let hwnd: HWND = GetForegroundWindow();
        if hwnd.is_invalid() {
            println!("[active_app] no foreground window");
            return None;
        }

        // Window title
        let title_len = GetWindowTextLengthW(hwnd);
        let window_title = if title_len > 0 {
            let mut buf = vec![0u16; (title_len as usize) + 1];
            let written = GetWindowTextW(hwnd, &mut buf);
            String::from_utf16_lossy(&buf[..written as usize])
        } else {
            String::new()
        };

        // PID for the foreground window
        let mut pid: u32 = 0;
        let _tid = GetWindowThreadProcessId(hwnd, Some(&mut pid as *mut u32));
        if pid == 0 {
            println!("[active_app] could not resolve PID for foreground window");
            return Some(ActiveAppContext {
                window_title,
                ..Default::default()
            });
        }

        let executable_path = match OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) {
            Ok(handle) => {
                let mut buf = vec![0u16; MAX_PATH as usize];
                let mut size: u32 = buf.len() as u32;
                let result = QueryFullProcessImageNameW(
                    handle,
                    PROCESS_NAME_FORMAT(0),
                    windows::core::PWSTR(buf.as_mut_ptr()),
                    &mut size,
                );
                let _ = CloseHandle(handle);
                if result.is_ok() && size > 0 {
                    String::from_utf16_lossy(&buf[..size as usize])
                } else {
                    String::new()
                }
            }
            Err(e) => {
                println!("[active_app] OpenProcess(pid={pid}) failed: {e:?}");
                String::new()
            }
        };

        let process_name = executable_path
            .rsplit(|c| c == '\\' || c == '/')
            .next()
            .unwrap_or("")
            .to_string();

        let ctx = ActiveAppContext {
            process_name,
            executable_path,
            window_title,
            pid,
        };

        println!(
            "[active_app] foreground: pid={} process={:?} title={:?}",
            ctx.pid, ctx.process_name, ctx.window_title
        );

        Some(ctx)
    }
}

#[cfg(not(target_os = "windows"))]
pub fn detect_active_app() -> Option<ActiveAppContext> {
    // PRD says Windows-first; on other platforms we silently fall through
    // so the caller routes to the default questionnaire flow.
    None
}
