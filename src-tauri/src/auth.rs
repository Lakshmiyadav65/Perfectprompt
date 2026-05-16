use serde::Serialize;
use tauri::State;

use crate::AppState;

/// Push the current Supabase session JWT from JS into Rust AppState.
/// Called by `useAuth` whenever Supabase emits an auth state change with
/// a valid session, and after a refresh. Empty/expired tokens should be
/// cleared via `clear_session_token` instead.
#[tauri::command]
pub fn set_session_token(token: String, state: State<AppState>) -> Result<(), String> {
    if token.is_empty() {
        return Err("token must not be empty".into());
    }
    let mut guard = state
        .session_token
        .lock()
        .map_err(|e| format!("session_token mutex poisoned: {e}"))?;
    *guard = Some(token);
    Ok(())
}

/// Clear the stored session token (sign-out, expiry without refresh).
/// After this, the pipeline falls back to the BYOK path.
#[tauri::command]
pub fn clear_session_token(state: State<AppState>) -> Result<(), String> {
    let mut guard = state
        .session_token
        .lock()
        .map_err(|e| format!("session_token mutex poisoned: {e}"))?;
    *guard = None;
    Ok(())
}

#[derive(Serialize)]
pub struct AuthStatus {
    pub signed_in: bool,
}

/// Lightweight check from JS to confirm Rust sees a session. Used by
/// the Account section in Settings to recover after a window reload.
#[tauri::command]
pub fn get_auth_status(state: State<AppState>) -> Result<AuthStatus, String> {
    let guard = state
        .session_token
        .lock()
        .map_err(|e| format!("session_token mutex poisoned: {e}"))?;
    Ok(AuthStatus {
        signed_in: guard.is_some(),
    })
}

/// Internal helper for the pipeline. Returns `Some(token)` if the user
/// is currently signed in and we should route via the hosted /enhance
/// edge function; `None` falls through to the existing Groq BYOK path.
pub fn current_token(state: &AppState) -> Option<String> {
    state.session_token.lock().ok().and_then(|g| g.clone())
}

// Note on the clone above: lock() returns MutexGuard<Option<String>>;
// the `and_then(|g| g.clone())` invokes Option<String>::clone via deref
// coercion of the guard, producing Option<String>. That's the value we
// want — a snapshot of the token at the time of the read.
