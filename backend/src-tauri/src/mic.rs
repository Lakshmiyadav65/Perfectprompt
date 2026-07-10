//! Mic — voice-to-perfect-prompt, the audio twin of Visual Annotate.
//!
//! Where Annotate captures the *screen* and Enhance captures the *clipboard
//! selection*, Mic captures the user's *voice*. Wispr-Flow-style: hold a
//! push-to-talk hotkey, speak, release — the words come back as clean text at
//! the cursor. Because this is PerfectPrompt (not a plain dictation app), the
//! transcript is then run through the exact same enhancement engine the hotkey
//! flow uses, so rough speech becomes a structured, high-quality prompt.
//!
//! Two chords pick the mode (mirroring the existing `Shift+Alt+E` bypass idiom
//! in `hotkey.rs`):
//!
//!   * `Alt+M`        → [`MicMode::Enhance`]  → `pipeline::run`        (perfect prompt)
//!   * `Shift+Alt+M`  → [`MicMode::Dictate`]  → `pipeline::run_polish` (clean dictation)
//!
//! Audio capture itself lives in the webview (`getUserMedia` + `MediaRecorder`
//! in `MicOverlay.tsx`) for the same reason Annotate composites pins in the
//! frontend: the browser already has a first-class media stack, so we don't
//! add a native audio dependency. The Rust side owns the lifecycle (show the
//! listening pill, drive start/stop over events) and the Groq calls:
//!
//!   1. On the hotkey `Pressed`, [`begin`] shows the non-focusable listening
//!      pill and emits `mic://start` with the chosen mode.
//!   2. The overlay records until it receives `mic://stop` (emitted by [`end`]
//!      on the hotkey `Released`), then hands the audio back as base64.
//!   3. [`transcribe_and_enhance`] sends the audio to Groq Whisper, runs the
//!      transcript through the enhance (or polish) pipeline, and pastes the
//!      result at the cursor via `clipboard::replace_selection`.
//!
//! v1 is BYOK-only (the user's own Groq key via [`crate::enhance::load_api_key`]),
//! consistent with Annotate — the hosted/Supabase path stays text-only for now.

use std::time::Duration;

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, PhysicalPosition, Runtime};

use crate::AppState;

/// Window label for the floating "listening" pill. Declared in
/// `tauri.conf.json` and kept alive across close in `lib.rs`.
const MIC_LABEL: &str = "mic";

const TRANSCRIBE_URL: &str = "https://api.groq.com/openai/v1/audio/transcriptions";
/// Groq's fast, cheap Whisper. `turbo` is ~real-time and accurate enough for
/// short push-to-talk clips; kept as a single const so a model swap is a
/// one-line edit (mirrors `annotate::VISION_MODEL`).
const TRANSCRIBE_MODEL: &str = "whisper-large-v3-turbo";
/// Whisper calls upload an audio blob, so they get a longer budget than the
/// 30s text path (matches annotate's 45s vision budget).
const REQUEST_TIMEOUT_SECS: u64 = 45;
/// Guard against transcribing near-silent accidental taps. Anything shorter
/// than this many base64 chars (~a few KB of Opus) is almost certainly an
/// empty press, not speech.
const MIN_AUDIO_B64_LEN: usize = 2_000;

// ─────────────────────────────────────────────────────────────────────
// Mode
// ─────────────────────────────────────────────────────────────────────

/// Which pipeline the transcript is routed through. Chosen by the hotkey
/// chord (`Alt+M` vs `Shift+Alt+M`) and echoed to the overlay so it can label
/// the listening pill.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MicMode {
    /// Speech → full enhancement pipeline → a structured "perfect prompt".
    Enhance,
    /// Speech → voice-preserving polish → clean, readable dictation.
    Dictate,
}

impl MicMode {
    pub fn as_str(self) -> &'static str {
        match self {
            MicMode::Enhance => "enhance",
            MicMode::Dictate => "dictate",
        }
    }
}

/// Payload for the `mic://start` event so the overlay knows which mode it's
/// recording for (drives the pill label + which command arg to send back).
#[derive(Debug, Clone, Serialize)]
struct MicStartPayload {
    mode: &'static str,
}

// ─────────────────────────────────────────────────────────────────────
// Session state (lives on AppState)
// ─────────────────────────────────────────────────────────────────────

/// Tracks whether a push-to-talk capture is currently in flight. Held behind a
/// `Mutex` on `AppState`. The hotkey handler fires on every key repeat while a
/// chord is held, so `recording` de-dupes repeat `Pressed` events; the
/// staggered release of a two-key chord (e.g. Shift released before Alt+M) can
/// momentarily re-match the single-modifier chord, so a stopped session is
/// briefly "cooling down" to swallow that spurious re-trigger.
#[derive(Debug, Default)]
pub struct MicSession {
    pub recording: bool,
    pub mode: Option<MicMode>,
    /// Set to a short future instant when a capture stops; `begin` ignores new
    /// starts until it passes. `None` once no cooldown is pending.
    pub cooldown_until: Option<std::time::Instant>,
}

/// How long after a stop we ignore fresh starts, to absorb staggered chord
/// release. Short enough to never block a genuine second press.
const COOLDOWN_MS: u64 = 450;

// ─────────────────────────────────────────────────────────────────────
// Lifecycle — driven by the global hotkey (see hotkey::register)
// ─────────────────────────────────────────────────────────────────────

/// Begin a push-to-talk capture in `mode`: show the listening pill and tell
/// the overlay to start recording. Idempotent under key-repeat and the
/// post-stop cooldown, so holding the chord only ever starts one capture.
pub fn begin<R: Runtime>(app: &AppHandle<R>, mode: MicMode) -> Result<()> {
    {
        let state = app.state::<AppState>();
        let mut session = state.mic.lock().unwrap();
        if session.recording {
            // Key-repeat while already recording — ignore.
            return Ok(());
        }
        if let Some(until) = session.cooldown_until {
            if std::time::Instant::now() < until {
                // Spurious re-trigger from a staggered chord release — ignore.
                return Ok(());
            }
        }
        session.recording = true;
        session.mode = Some(mode);
        session.cooldown_until = None;
    }

    // Hide the command bar so the pill doesn't overlap it.
    if let Some(cb) = app.get_webview_window("command-bar") {
        let _ = cb.hide();
    }

    let window = app
        .get_webview_window(MIC_LABEL)
        .ok_or_else(|| anyhow!("mic window not found"))?;
    position_bottom_center(&window);
    window.show()?;

    app.emit_to(
        MIC_LABEL,
        "mic:start",
        MicStartPayload { mode: mode.as_str() },
    )
    .map_err(|e| anyhow!("emit mic:start failed: {e}"))?;

    println!("[mic] begin — mode={}", mode.as_str());
    Ok(())
}

/// End the capture: tell the overlay to stop recording and hand the audio
/// back. Clears the recording flag and opens the cooldown window. Called on
/// the hotkey `Released`. No-op if we weren't recording.
pub fn end<R: Runtime>(app: &AppHandle<R>) {
    {
        let state = app.state::<AppState>();
        let mut session = state.mic.lock().unwrap();
        if !session.recording {
            return;
        }
        session.recording = false;
        session.cooldown_until =
            Some(std::time::Instant::now() + Duration::from_millis(COOLDOWN_MS));
    }
    if let Err(e) = app.emit_to(MIC_LABEL, "mic:stop", ()) {
        eprintln!("[mic] emit mic:stop failed: {e}");
    }
    println!("[mic] end — stop requested");
}

/// Center the pill horizontally, docked a little above the bottom of the
/// primary monitor (Wispr-style). Best-effort — a positioning failure just
/// leaves the window wherever it was.
fn position_bottom_center<R: Runtime>(window: &tauri::WebviewWindow<R>) {
    let Ok(Some(monitor)) = window.primary_monitor() else {
        return;
    };
    let size = monitor.size();
    let pos = monitor.position();
    let scale = monitor.scale_factor();
    let Ok(win) = window.outer_size() else { return };

    let x = pos.x + ((size.width as i32 - win.width as i32) / 2);
    // 90 logical px above the bottom edge.
    let y = pos.y + size.height as i32 - win.height as i32 - ((90.0 * scale) as i32);
    let _ = window.set_position(PhysicalPosition::new(x, y));
}

// ─────────────────────────────────────────────────────────────────────
// Tauri commands
// ─────────────────────────────────────────────────────────────────────

/// Command form of [`begin`] used by the command-bar's Mic button (mouse
/// users, no hotkey). Always Enhance mode — the flagship behaviour. The button
/// is a click-toggle: a second press while recording routes to [`stop_mic`].
#[tauri::command]
pub fn start_mic<R: Runtime>(app: AppHandle<R>) -> std::result::Result<(), String> {
    begin(&app, MicMode::Enhance).map_err(|e| format!("{e:#}"))
}

/// Command form of [`end`] — used by the command-bar toggle and the overlay's
/// own Stop button so mouse users can end a capture without a key release.
#[tauri::command]
pub fn stop_mic<R: Runtime>(app: AppHandle<R>) {
    end(&app);
}

/// Whether a capture is currently in flight — lets the command-bar Mic button
/// render a record/stop state and decide which of start/stop to call.
#[tauri::command]
pub fn mic_is_recording<R: Runtime>(app: AppHandle<R>) -> bool {
    app.state::<AppState>().mic.lock().unwrap().recording
}

/// Hide the listening pill and re-float the command bar if the master toggle
/// is on. Called by the overlay when it's done showing a result/error, and on
/// cancel. Also clears any half-open session so a cancelled capture can't wedge
/// the recording flag.
#[tauri::command]
pub fn close_mic<R: Runtime>(app: AppHandle<R>) -> std::result::Result<(), String> {
    {
        let state = app.state::<AppState>();
        let mut session = state.mic.lock().unwrap();
        session.recording = false;
    }
    if let Some(w) = app.get_webview_window(MIC_LABEL) {
        let _ = w.hide();
    }
    if crate::settings::load(&app).enabled {
        let _ = crate::command_bar::show(&app);
    }
    Ok(())
}

/// The heart of Mic: transcribe the recorded audio and route the transcript
/// through the enhance (or polish) pipeline, then paste the result at the
/// cursor. Returns the final text so the overlay can show a confirmation.
///
/// `audio_base64` is the bare base64 (no `data:` prefix) of the recorded blob;
/// `mime` is its MIME type (e.g. `audio/webm`) so we can name the upload part
/// correctly for Whisper. `mode` selects the pipeline.
#[tauri::command]
pub async fn transcribe_and_enhance<R: Runtime>(
    app: AppHandle<R>,
    audio_base64: String,
    mime: String,
    mode: MicMode,
) -> std::result::Result<String, String> {
    transcribe_inner(&app, audio_base64, mime, mode)
        .await
        .map_err(|e| format!("{e:#}"))
}

async fn transcribe_inner<R: Runtime>(
    app: &AppHandle<R>,
    audio_base64: String,
    mime: String,
    mode: MicMode,
) -> Result<String> {
    if audio_base64.len() < MIN_AUDIO_B64_LEN {
        return Err(anyhow!(
            "Didn't catch that — hold the key a little longer and speak."
        ));
    }

    let audio = {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD
            .decode(audio_base64.as_bytes())
            .map_err(|e| anyhow!("audio was not valid base64: {e}"))?
    };

    let transcript = transcribe(app, audio, &mime).await?;
    let transcript = transcript.trim().to_string();
    if transcript.is_empty() {
        return Err(anyhow!("Didn't catch any speech — try again."));
    }
    println!(
        "[mic] transcript ({} chars) → {} pipeline",
        transcript.chars().count(),
        mode.as_str()
    );

    // Detect the app the user was dictating into. The push-to-talk keypress
    // never stole focus, so the foreground is still the user's target window —
    // this keeps the trace/history record and cache fingerprint accurate.
    let active_app = crate::active_app::detect_active_app().unwrap_or_default();
    let pi = crate::pipeline::PipelineInput {
        raw_input: transcript,
        active_app: active_app.process_name.clone(),
    };

    let output = match mode {
        MicMode::Enhance => crate::pipeline::run(app, pi).await,
        MicMode::Dictate => crate::pipeline::run_polish(app, pi).await,
    }
    .map_err(|e| anyhow!("pipeline failed: {e:#}"))?;

    // Paste at the cursor. The listening pill is non-focusable (like the status
    // pill shown during a normal enhance), so the user's field keeps focus and
    // the synthetic Ctrl+V lands there.
    crate::clipboard::replace_selection(app, &output.final_text)
        .await
        .map_err(|e| anyhow!("paste failed: {e}"))?;

    if output.used_fallback {
        if let Some(reason) = &output.fallback_reason {
            crate::tray::notify_fallback(app, reason);
        }
    }

    println!("[mic] pasted {} chars", output.final_text.len());
    Ok(output.final_text)
}

// ─────────────────────────────────────────────────────────────────────
// Groq Whisper (BYOK, multipart upload)
// ─────────────────────────────────────────────────────────────────────

async fn transcribe<R: Runtime>(app: &AppHandle<R>, audio: Vec<u8>, mime: &str) -> Result<String> {
    let api_key = crate::enhance::load_api_key(app).map_err(|e| anyhow!("{e:#}"))?;

    // Name the part with an extension Groq recognises for the container. The
    // MediaRecorder default is webm/opus; fall back to a generic name.
    let filename = filename_for_mime(mime);
    let part = reqwest::multipart::Part::bytes(audio)
        .file_name(filename)
        .mime_str(mime)
        .map_err(|e| anyhow!("bad audio mime {mime:?}: {e}"))?;

    let form = reqwest::multipart::Form::new()
        .text("model", TRANSCRIBE_MODEL)
        .text("response_format", "json")
        .text("temperature", "0")
        // Bias Whisper toward English; drop this to auto-detect if we ever
        // localise. Kept explicit so short clips aren't misdetected.
        .text("language", "en")
        .part("file", part);

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS))
        .build()
        .map_err(|e| anyhow!("http client build: {e}"))?;

    let resp = client
        .post(TRANSCRIBE_URL)
        .bearer_auth(&api_key)
        .multipart(form)
        .send()
        .await
        .map_err(|e| anyhow!("transcription request failed: {e}"))?;

    let status = resp.status();
    let text = resp
        .text()
        .await
        .map_err(|e| anyhow!("transcription body read failed: {e}"))?;

    if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        return Err(anyhow!(
            "Groq rate limit hit while transcribing — wait a moment and try again."
        ));
    }
    if !status.is_success() {
        return Err(anyhow!(
            "Groq transcription returned {}: {}",
            status,
            trim_for_log(&text, 300)
        ));
    }

    let parsed: TranscriptionResponse = serde_json::from_str(&text)
        .map_err(|e| anyhow!("transcription response not JSON: {e}: {}", trim_for_log(&text, 200)))?;
    Ok(parsed.text)
}

#[derive(Deserialize)]
struct TranscriptionResponse {
    text: String,
}

/// Map a recorder MIME type to a filename with the extension Groq's Whisper
/// endpoint keys the decoder off. Defaults to webm — the MediaRecorder default.
fn filename_for_mime(mime: &str) -> &'static str {
    let base = mime.split(';').next().unwrap_or("").trim();
    match base {
        "audio/webm" => "audio.webm",
        "audio/ogg" => "audio.ogg",
        "audio/mp4" | "audio/mpeg" => "audio.mp4",
        "audio/wav" | "audio/x-wav" => "audio.wav",
        _ => "audio.webm",
    }
}

fn trim_for_log(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_round_trips_through_serde() {
        // The overlay echoes the mode string back into `transcribe_and_enhance`
        // — make sure the wire form matches what `begin` emits.
        assert_eq!(MicMode::Enhance.as_str(), "enhance");
        assert_eq!(MicMode::Dictate.as_str(), "dictate");
        let e: MicMode = serde_json::from_str("\"enhance\"").unwrap();
        let d: MicMode = serde_json::from_str("\"dictate\"").unwrap();
        assert_eq!(e, MicMode::Enhance);
        assert_eq!(d, MicMode::Dictate);
    }

    #[test]
    fn filename_matches_recorder_containers() {
        assert_eq!(filename_for_mime("audio/webm;codecs=opus"), "audio.webm");
        assert_eq!(filename_for_mime("audio/ogg"), "audio.ogg");
        assert_eq!(filename_for_mime("audio/mp4"), "audio.mp4");
        // Unknown containers fall back to the MediaRecorder default.
        assert_eq!(filename_for_mime("application/octet-stream"), "audio.webm");
    }
}
