//! Visual Annotate — the desktop-native answer to Agentation.
//!
//! Agentation lives inside the browser DOM: you click a page element and it
//! emits structured context (selector, source path, React tree) for a coding
//! agent. PerfectPrompt lives on the desktop and works across every app, so
//! we can't read the DOM — instead we:
//!
//!   1. Freeze-capture the monitor under the cursor (native Win32 GDI).
//!   2. Show a full-screen overlay (`#/annotate`) where the user drops
//!      numbered pins on the UI and types feedback per pin.
//!   3. Composite the pins onto the screenshot (done frontend-side) and send
//!      that annotated image + the notes + the ACTIVE PROJECT's file-index to
//!      a Groq vision model.
//!   4. The model emits Agentation-style structured markdown — per pin: the
//!      element, its most-likely source file (grounded in the project's
//!      `<file_index>`, never invented), and the requested change — which we
//!      drop on the clipboard for the user to paste into Claude Code / Cursor.
//!
//! The "which source file?" intelligence Agentation gets from source maps we
//! get from the existing Project Knowledge digest: [`crate::pipeline::
//! build_context_block`] hands us the same `<project_summary>` + `<file_index>`
//! the enhance pipeline injects.
//!
//! v1 is BYOK-only (the user's own Groq key via [`crate::enhance::
//! load_api_key`]); the hosted/Supabase path stays text-only for now.

use std::time::Duration;

use anyhow::{anyhow, Result};
use serde::Deserialize;
use tauri::{AppHandle, Manager, PhysicalPosition, PhysicalSize, Runtime};

use crate::AppState;

/// Window label for the full-screen annotate overlay. Declared in
/// `tauri.conf.json` and kept alive across close in `lib.rs`.
const ANNOTATE_LABEL: &str = "annotate";

/// Bundled system prompt for the vision call. Resolved via the same
/// `load_prompt` resource path as the enhancer prompts, so dev + installed
/// builds both find it.
const ANNOTATE_PROMPT_FILE: &str = "annotate-enhancer.md";

const API_URL: &str = "https://api.groq.com/openai/v1/chat/completions";
/// Groq multimodal model. Kept as a single const so a Groq model-id change
/// is a one-line edit. Llama-4 Scout is Groq's current vision workhorse; the
/// older `llama-3.2-*-vision` previews were decommissioned.
const VISION_MODEL: &str = "meta-llama/llama-4-scout-17b-16e-instruct";
const VISION_MAX_TOKENS: u32 = 1500;
const VISION_TEMPERATURE: f32 = 0.3;
/// Vision calls carry a base64 image + can produce a long structured answer,
/// so they get a longer budget than the 30s text path.
const REQUEST_TIMEOUT_SECS: u64 = 45;

/// One annotation pin as sent from the overlay. `n` is the 1-based label the
/// user sees (and that is drawn on the composited image); `note` is their
/// free-text feedback for that spot (may be empty).
#[derive(Debug, Deserialize)]
pub struct Pin {
    pub n: u32,
    pub note: String,
}

// ─────────────────────────────────────────────────────────────────────
// Entry point — begin an annotation session
// ─────────────────────────────────────────────────────────────────────

/// Capture the monitor under the cursor and raise the overlay. Shared by the
/// global hotkey (see `hotkey::register`) and the `start_annotation` command
/// (the floating command-bar button).
pub async fn begin<R: Runtime>(app: &AppHandle<R>) -> Result<()> {
    // Hide our own on-screen chrome so it isn't baked into the screenshot.
    for label in ["command-bar", "status", "toast", ANNOTATE_LABEL] {
        if let Some(w) = app.get_webview_window(label) {
            let _ = w.hide();
        }
    }
    // Let the compositor actually remove those windows before we grab pixels.
    tokio::time::sleep(Duration::from_millis(120)).await;

    let (left, top, width, height) = target_monitor_rect();
    let png = capture_rect(left, top, width, height)?;
    let b64 = {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD.encode(&png)
    };
    let data_url = format!("data:image/png;base64,{b64}");
    println!(
        "[annotate] captured {width}x{height} at ({left},{top}) — {} KB png",
        png.len() / 1024
    );

    {
        let state = app.state::<AppState>();
        *state.pending_capture.lock().unwrap() = Some(data_url);
    }

    let window = app
        .get_webview_window(ANNOTATE_LABEL)
        .ok_or_else(|| anyhow!("annotate window not found"))?;

    // Fresh mount each session so old pins don't linger.
    let _ = window.eval("window.location.hash = '#/annotate'; window.location.reload();");
    tokio::time::sleep(Duration::from_millis(140)).await;

    // Cover exactly the monitor we captured (physical pixels — the capture
    // rect is already physical).
    let _ = window.set_position(PhysicalPosition::new(left, top));
    let _ = window.set_size(PhysicalSize::new(width.max(1) as u32, height.max(1) as u32));
    window.show()?;
    window.set_focus()?;
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────
// Tauri commands
// ─────────────────────────────────────────────────────────────────────

/// Command form of [`begin`], used by the command-bar's Annotate button.
#[tauri::command]
pub async fn start_annotation<R: Runtime>(app: AppHandle<R>) -> std::result::Result<(), String> {
    begin(&app).await.map_err(|e| format!("{e:#}"))
}

/// Hand the freshly-captured screenshot (a `data:image/png;base64,...` URL)
/// to the overlay so it can render it and let the user annotate on top.
#[tauri::command]
pub fn get_annotation_capture<R: Runtime>(app: AppHandle<R>) -> Option<String> {
    app.state::<AppState>()
        .pending_capture
        .lock()
        .unwrap()
        .clone()
}

/// Re-copy the given text to the clipboard. Backs the overlay's "Copy again"
/// button (the initial copy happens inside `run_annotation`).
#[tauri::command]
pub fn copy_text<R: Runtime>(app: AppHandle<R>, text: String) -> std::result::Result<(), String> {
    write_clipboard(&app, &text).map_err(|e| format!("{e:#}"))
}

/// Close the overlay: hide it, drop the captured image from memory, and
/// re-float the command bar if the master toggle is on. Called for both
/// Cancel (Escape) and Done.
#[tauri::command]
pub fn close_annotation<R: Runtime>(app: AppHandle<R>) -> std::result::Result<(), String> {
    if let Some(w) = app.get_webview_window(ANNOTATE_LABEL) {
        let _ = w.hide();
    }
    *app.state::<AppState>().pending_capture.lock().unwrap() = None;
    if crate::settings::load(&app).enabled {
        let _ = crate::command_bar::show(&app);
    }
    Ok(())
}

/// Run the vision model over the composited (pins-drawn) image + notes and
/// return the structured markdown. Also drops it on the clipboard so the
/// user can paste straight into their agent. The overlay stays up (showing
/// the result); `close_annotation` tears it down.
#[tauri::command]
pub async fn run_annotation<R: Runtime>(
    app: AppHandle<R>,
    image_data_url: String,
    pins: Vec<Pin>,
) -> std::result::Result<String, String> {
    run_inner(&app, image_data_url, pins)
        .await
        .map_err(|e| format!("{e:#}"))
}

async fn run_inner<R: Runtime>(
    app: &AppHandle<R>,
    image_data_url: String,
    pins: Vec<Pin>,
) -> Result<String> {
    if pins.is_empty() {
        return Err(anyhow!("Add at least one pin before generating."));
    }

    let system_prompt = crate::enhance::load_prompt(app, ANNOTATE_PROMPT_FILE)
        .map_err(|e| anyhow!("could not load annotate prompt: {e:#}"))?;

    // Reuse the enhance pipeline's context builder so annotate gets the exact
    // same <project_summary> + <file_index> the text pipeline injects. This
    // is what lets the model name a likely source file instead of guessing.
    let active = crate::projects::active_project_for(app);
    let context = crate::pipeline::build_context_block(app, active.as_ref(), "annotate");

    let user_text = build_user_text(&pins, context.as_deref());
    let markdown = call_vision_llm(app, &system_prompt, &user_text, &image_data_url).await?;

    // Copy to clipboard so "paste into your agent" is a single Ctrl+V. We do
    // NOT synthesize a paste (unlike the enhance path) — the target is the
    // user's agent chat, which they focus themselves.
    if let Err(e) = write_clipboard(app, &markdown) {
        eprintln!("[annotate] clipboard write failed (non-fatal): {e:#}");
    }

    println!("[annotate] generated {} chars for {} pin(s)", markdown.len(), pins.len());
    Ok(markdown)
}

// ─────────────────────────────────────────────────────────────────────
// Vision LLM call (BYOK Groq, multimodal)
// ─────────────────────────────────────────────────────────────────────

fn build_user_text(pins: &[Pin], context: Option<&str>) -> String {
    let mut t = String::new();
    t.push_str(
        "I captured a screenshot of a running UI and dropped numbered pins on \
         the elements I want changed. Each pin is drawn on the attached image \
         as a circled number. Here is my feedback for each pin:\n\n",
    );
    for p in pins {
        let note = p.note.trim();
        if note.is_empty() {
            t.push_str(&format!(
                "{}. (no note — infer the most likely intended change from the element)\n",
                p.n
            ));
        } else {
            t.push_str(&format!("{}. {}\n", p.n, note));
        }
    }

    match context.filter(|s| !s.trim().is_empty()) {
        Some(ctx) => {
            t.push_str(
                "\nThe user is working in the project described below. Use the \
                 <file_index> to map each pin to its most likely source file(s) — \
                 never invent a path that isn't in the index.\n\n",
            );
            t.push_str(ctx);
        }
        None => {
            t.push_str(
                "\nNo project is linked, so you cannot cite exact source files. \
                 Describe each element precisely instead so a developer can locate \
                 it from the description alone.\n",
            );
        }
    }

    t.push_str("\n\nNow produce the structured markdown exactly per your instructions.");
    t
}

async fn call_vision_llm<R: Runtime>(
    app: &AppHandle<R>,
    system_prompt: &str,
    user_text: &str,
    image_data_url: &str,
) -> Result<String> {
    let api_key = crate::enhance::load_api_key(app)
        .map_err(|e| anyhow!("{e:#}"))?;

    let body = serde_json::json!({
        "model": VISION_MODEL,
        "max_tokens": VISION_MAX_TOKENS,
        "temperature": VISION_TEMPERATURE,
        "messages": [
            { "role": "system", "content": system_prompt },
            { "role": "user", "content": [
                { "type": "text", "text": user_text },
                { "type": "image_url", "image_url": { "url": image_data_url } }
            ]}
        ]
    });

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS))
        .build()
        .map_err(|e| anyhow!("http client build: {e}"))?;

    let resp = client
        .post(API_URL)
        .bearer_auth(&api_key)
        .header("content-type", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| anyhow!("vision request failed: {e}"))?;

    let status = resp.status();
    let text = resp
        .text()
        .await
        .map_err(|e| anyhow!("vision body read failed: {e}"))?;

    if !status.is_success() {
        return Err(anyhow!(
            "Groq vision call returned {}: {}",
            status,
            trim_for_log(&text, 300)
        ));
    }

    let parsed: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| anyhow!("vision response not JSON: {e}: {}", trim_for_log(&text, 200)))?;
    let content = parsed["choices"][0]["message"]["content"]
        .as_str()
        .ok_or_else(|| anyhow!("no text content in vision response"))?
        .trim()
        .to_string();
    if content.is_empty() {
        return Err(anyhow!("vision model returned empty content"));
    }
    Ok(content)
}

fn write_clipboard<R: Runtime>(app: &AppHandle<R>, text: &str) -> Result<()> {
    use tauri_plugin_clipboard_manager::ClipboardExt;
    app.clipboard()
        .write_text(text.to_string())
        .map_err(|e| anyhow!("{e}"))
}

fn trim_for_log(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
}

// ─────────────────────────────────────────────────────────────────────
// Native screen capture (Win32 GDI)
// ─────────────────────────────────────────────────────────────────────
//
// We capture with GDI directly rather than a screenshot crate: the app
// already depends on the exact `windows` features we need (Graphics_Gdi +
// UI_WindowsAndMessaging), and toast_window.rs already uses the same
// MonitorFromPoint/GetMonitorInfoW pattern to reason in physical pixels — so
// this stays consistent with the codebase and avoids a version-drifting dep.

/// Physical-pixel rect (left, top, width, height) of the monitor under the
/// cursor. Falls back to a 1080p primary guess if the query fails.
#[cfg(target_os = "windows")]
fn target_monitor_rect() -> (i32, i32, i32, i32) {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTONEAREST,
    };
    use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;
    unsafe {
        let mut pt = POINT { x: 0, y: 0 };
        let _ = GetCursorPos(&mut pt);
        let monitor = MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if GetMonitorInfoW(monitor, &mut info).as_bool() {
            let r = info.rcMonitor;
            (r.left, r.top, r.right - r.left, r.bottom - r.top)
        } else {
            (0, 0, 1920, 1080)
        }
    }
}

/// BitBlt the given physical rect off the virtual-desktop DC and return it as
/// PNG bytes. The screen DC (`GetDC(None)`) spans the whole virtual desktop,
/// so negative/offset source coords for secondary monitors work.
#[cfg(target_os = "windows")]
fn capture_rect(left: i32, top: i32, width: i32, height: i32) -> Result<Vec<u8>> {
    use std::ffi::c_void;
    use windows::Win32::Graphics::Gdi::{
        BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC,
        GetDIBits, ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB,
        DIB_RGB_COLORS, HGDIOBJ, SRCCOPY,
    };

    if width <= 0 || height <= 0 {
        return Err(anyhow!("invalid capture size {width}x{height}"));
    }

    unsafe {
        let screen_dc = GetDC(None);
        if screen_dc.is_invalid() {
            return Err(anyhow!("GetDC(screen) failed"));
        }
        let mem_dc = CreateCompatibleDC(Some(screen_dc));
        let bitmap = CreateCompatibleBitmap(screen_dc, width, height);
        let old = SelectObject(mem_dc, HGDIOBJ(bitmap.0));

        let blt = BitBlt(
            mem_dc,
            0,
            0,
            width,
            height,
            Some(screen_dc),
            left,
            top,
            SRCCOPY,
        );

        let mut bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                // Negative height → top-down rows, so buffer order matches
                // what image::PngEncoder expects (no vertical flip needed).
                biHeight: -height,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0 as u32,
                ..Default::default()
            },
            ..Default::default()
        };

        let mut buf = vec![0u8; (width as usize) * (height as usize) * 4];
        let scanlines = GetDIBits(
            mem_dc,
            bitmap,
            0,
            height as u32,
            Some(buf.as_mut_ptr() as *mut c_void),
            &mut bmi,
            DIB_RGB_COLORS,
        );

        // Release GDI objects before bailing on any error.
        SelectObject(mem_dc, old);
        let _ = DeleteObject(HGDIOBJ(bitmap.0));
        let _ = DeleteDC(mem_dc);
        ReleaseDC(None, screen_dc);

        if blt.is_err() {
            return Err(anyhow!("BitBlt failed: {blt:?}"));
        }
        if scanlines == 0 {
            return Err(anyhow!("GetDIBits returned 0 scanlines"));
        }

        // GDI hands back BGRA; PNG wants RGBA. Swap R/B and force opaque
        // alpha (the desktop DC's alpha channel is meaningless).
        for px in buf.chunks_exact_mut(4) {
            px.swap(0, 2);
            px[3] = 255;
        }

        encode_png(&buf, width as u32, height as u32)
    }
}

#[cfg(not(target_os = "windows"))]
fn target_monitor_rect() -> (i32, i32, i32, i32) {
    (0, 0, 1920, 1080)
}

#[cfg(not(target_os = "windows"))]
fn capture_rect(_left: i32, _top: i32, _width: i32, _height: i32) -> Result<Vec<u8>> {
    Err(anyhow!(
        "screen capture for Visual Annotate is only implemented on Windows in v1"
    ))
}

fn encode_png(rgba: &[u8], width: u32, height: u32) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder
            .write_header()
            .map_err(|e| anyhow!("png header: {e}"))?;
        writer
            .write_image_data(rgba)
            .map_err(|e| anyhow!("png write: {e}"))?;
    }
    Ok(out)
}
