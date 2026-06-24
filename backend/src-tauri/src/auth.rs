use serde::Serialize;
use tauri::{AppHandle, Runtime, State};

use crate::AppState;

/// Push the current Supabase session JWT + user uuid from JS into Rust
/// AppState. Called by `useAuth` whenever Supabase emits an auth state
/// change with a valid session, and on the post-OAuth callback. Empty
/// tokens should go through `clear_session_token` instead.
///
/// `user_id` is the Supabase auth.users.id (uuid string) for the user
/// whose token this is. Stored separately from the token so the
/// settings layer (per-user API key scoping) can look up state by id
/// without having to parse the JWT every time.
///
/// Emits `settings:key-changed` after the swap so the API-key UI on the
/// React side re-fetches its state for the newly-signed-in user.
/// Without this signal, user A's "you have a key" UI would persist
/// after user B signs in until something else triggered a refresh.
#[tauri::command]
pub fn set_session_token<R: Runtime>(
    app: AppHandle<R>,
    token: String,
    user_id: String,
    state: State<AppState>,
) -> Result<(), String> {
    if token.is_empty() {
        return Err("token must not be empty".into());
    }
    if user_id.is_empty() {
        return Err("user_id must not be empty".into());
    }
    {
        let mut t = state
            .session_token
            .lock()
            .map_err(|e| format!("session_token mutex poisoned: {e}"))?;
        *t = Some(token);
    }
    {
        let mut u = state
            .current_user_id
            .lock()
            .map_err(|e| format!("current_user_id mutex poisoned: {e}"))?;
        *u = Some(user_id);
    }
    crate::settings::emit_key_changed(&app);
    Ok(())
}

/// Clear the stored session token AND the current user id. Called on
/// sign-out and on token expiry without refresh. After this, the
/// pipeline falls back to the BYOK path and the API-key UI shows the
/// "set up your key" prompt again — even if a previous user's key is
/// still on disk under their user_id (it just isn't readable anymore
/// without their auth).
#[tauri::command]
pub fn clear_session_token<R: Runtime>(
    app: AppHandle<R>,
    state: State<AppState>,
) -> Result<(), String> {
    {
        let mut t = state
            .session_token
            .lock()
            .map_err(|e| format!("session_token mutex poisoned: {e}"))?;
        *t = None;
    }
    {
        let mut u = state
            .current_user_id
            .lock()
            .map_err(|e| format!("current_user_id mutex poisoned: {e}"))?;
        *u = None;
    }
    crate::settings::emit_key_changed(&app);
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

/// Internal helper for the settings + enhance modules. Returns the
/// uuid string of the currently-signed-in user, or `None` when no user
/// is signed in (BYOK / dev mode). Used to scope API key storage so
/// keys never leak across accounts.
pub fn current_user_id(state: &AppState) -> Option<String> {
    state.current_user_id.lock().ok().and_then(|g| g.clone())
}
