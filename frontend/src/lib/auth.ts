import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { openUrl } from "@tauri-apps/plugin-opener";
import { onOpenUrl } from "@tauri-apps/plugin-deep-link";

import { REDIRECT_URL, isSupabaseConfigured, supabase } from "./supabase";

/// Start the Google OAuth flow.
///
/// Supabase PKCE flow: we call signInWithOAuth which generates an
/// authorization URL (and stashes a code_verifier in localStorage).
/// We open that URL in the user's default browser. Once they consent,
/// Supabase redirects to perfectprompt://auth/callback?code=xxx — which
/// the deep-link listener (installed at app startup) catches and feeds
/// to exchangeCodeForSession.
export async function signInWithGoogle(): Promise<void> {
  if (!isSupabaseConfigured) {
    throw new Error("Supabase not configured");
  }
  const { data, error } = await supabase.auth.signInWithOAuth({
    provider: "google",
    options: {
      redirectTo: REDIRECT_URL,
      skipBrowserRedirect: true,
    },
  });
  if (error) throw error;
  if (!data?.url) throw new Error("Supabase did not return an OAuth URL");
  await openUrl(data.url);
}

export async function signInWithGitHub(): Promise<void> {
  if (!isSupabaseConfigured) {
    throw new Error("Supabase not configured");
  }
  const { data, error } = await supabase.auth.signInWithOAuth({
    provider: "github",
    options: {
      redirectTo: REDIRECT_URL,
      skipBrowserRedirect: true,
    },
  });
  if (error) throw error;
  if (!data?.url) throw new Error("Supabase did not return an OAuth URL");
  await openUrl(data.url);
}

/// Email + password signup. Returns whether the user still needs to
/// confirm their email — true when Supabase's "Confirm email" toggle
/// is on for the project, in which case `signUp` returns no session.
/// Caller should show a "check your email" state instead of redirecting.
export async function signUpWithPassword(
  name: string,
  email: string,
  password: string,
): Promise<{ needsEmailConfirmation: boolean }> {
  if (!isSupabaseConfigured) {
    throw new Error("Account features aren't configured for this build.");
  }
  const { data, error } = await supabase.auth.signUp({
    email,
    password,
    options: {
      data: { full_name: name },
    },
  });
  if (error) throw new Error(friendlyAuthError(error.message));
  // When email confirmation is enabled in Supabase, `data.session` is
  // null and the user must click the magic link before the session
  // becomes real. When it's disabled, the session is returned inline.
  return { needsEmailConfirmation: data.session === null };
}

/// Email + password sign-in. On success the session is persisted by
/// supabase-js and `onAuthStateChange` will sync it into Rust via
/// the existing useAuth subscriber.
export async function signInWithPassword(
  email: string,
  password: string,
): Promise<void> {
  if (!isSupabaseConfigured) {
    throw new Error("Account features aren't configured for this build.");
  }
  const { error } = await supabase.auth.signInWithPassword({ email, password });
  if (error) throw new Error(friendlyAuthError(error.message));
}

/// Send a password-reset email. The link Supabase emails points at
/// our deep-link callback URL; clicking it boots PerfectPrompt back
/// into a "set a new password" flow (not yet wired — for now the
/// link will simply sign the user in, which is enough to recover
/// access).
export async function resetPasswordForEmail(email: string): Promise<void> {
  if (!isSupabaseConfigured) {
    throw new Error("Account features aren't configured for this build.");
  }
  const { error } = await supabase.auth.resetPasswordForEmail(email, {
    redirectTo: REDIRECT_URL,
  });
  if (error) throw new Error(friendlyAuthError(error.message));
}

/// Set a new password for the currently authenticated user. Called
/// from the PasswordRecovery screen after the user clicks a reset
/// email link and lands back in the app with an active session.
export async function updatePassword(newPassword: string): Promise<void> {
  if (!isSupabaseConfigured) {
    throw new Error("Account features aren't configured for this build.");
  }
  const { error } = await supabase.auth.updateUser({ password: newPassword });
  if (error) throw new Error(friendlyAuthError(error.message));
}

export async function signOut(): Promise<void> {
  await supabase.auth.signOut();
  try {
    await invoke("clear_session_token");
  } catch (e) {
    console.error("[auth] clear_session_token failed:", e);
  }
}

/// Translate Supabase's raw error messages into user-friendly strings.
/// The brief is explicit: "Do not show raw technical errors."
function friendlyAuthError(raw: string): string {
  const msg = raw.toLowerCase();
  if (msg.includes("invalid login credentials")) {
    return "Email or password is incorrect.";
  }
  if (msg.includes("user already registered") || msg.includes("already been registered")) {
    return "An account with this email already exists. Try signing in instead.";
  }
  if (msg.includes("email not confirmed")) {
    return "Check your email and click the confirmation link before signing in.";
  }
  if (msg.includes("password should be at least") || msg.includes("password is too short")) {
    return "Password must be at least 6 characters.";
  }
  if (msg.includes("signups not allowed") || msg.includes("signup is disabled")) {
    return "Email signups aren't enabled on this server. Try a social provider instead.";
  }
  if (msg.includes("rate limit") || msg.includes("too many requests")) {
    return "Too many attempts. Please wait a minute and try again.";
  }
  if (msg.includes("invalid email")) {
    return "Please enter a valid email address.";
  }
  if (msg.includes("network") || msg.includes("failed to fetch")) {
    return "We couldn't reach the server. Check your connection and try again.";
  }
  // Fallback — log the original for debugging, show a generic line.
  console.warn("[auth] unmapped supabase error:", raw);
  return "Something went wrong. Please try again.";
}

/// Wire the deep-link callback. Called once at app startup. Returns
/// an unsubscribe function — not strictly needed in a single-window
/// session, but kept for tidiness if we ever reload the listener.
///
/// Two sources of deep-link URLs on Windows:
///   1. `onOpenUrl` from the deep-link plugin — fires when the app is
///      already focused and the OS dispatches the URL via IPC.
///   2. `deep-link-from-argv` — emitted by the single-instance plugin
///      in lib.rs when a second `perfectprompt.exe` launch arrives with
///      the URL in argv. Without this bridge, the URL is silently
///      dropped because single-instance discards the new process.
export async function installDeepLinkHandler(): Promise<() => void> {
  const unsubPlugin = await onOpenUrl((urls) => {
    for (const url of urls) {
      void handleCallbackUrl(url);
    }
  });
  const unsubArgv = await listen<string>("deep-link-from-argv", (event) => {
    void handleCallbackUrl(event.payload);
  });
  return () => {
    unsubPlugin();
    unsubArgv();
  };
}

async function handleCallbackUrl(rawUrl: string): Promise<void> {
  try {
    const url = new URL(rawUrl);
    // Errors come back as ?error=...&error_description=...
    const errorCode = url.searchParams.get("error");
    if (errorCode) {
      const desc = url.searchParams.get("error_description") ?? errorCode;
      console.error("[auth] OAuth provider returned error:", desc);
      window.dispatchEvent(
        new CustomEvent("pf-auth-error", { detail: { message: desc } }),
      );
      return;
    }
    const code = url.searchParams.get("code");
    if (!code) {
      console.warn("[auth] callback URL missing 'code' param:", rawUrl);
      return;
    }
    // Supabase appends `type=recovery` to the redirect URL for
    // password-reset emails. We detect that BEFORE the exchange so
    // the UI can route the user to a "set a new password" screen
    // instead of dropping them into the main app with an unchanged
    // password.
    const isRecovery = url.searchParams.get("type") === "recovery";

    const { data, error } = await supabase.auth.exchangeCodeForSession(code);
    if (error) throw error;
    const token = data.session?.access_token;
    const userId = data.session?.user?.id;
    if (!token) throw new Error("exchangeCodeForSession returned no access_token");
    if (!userId) throw new Error("exchangeCodeForSession returned no user id");
    await invoke("set_session_token", { token, userId });

    if (isRecovery) {
      window.dispatchEvent(new CustomEvent("pf-recovery-mode"));
    }
  } catch (e) {
    const message = (e as Error)?.message ?? String(e);
    console.error("[auth] callback handling failed:", message);
    window.dispatchEvent(
      new CustomEvent("pf-auth-error", { detail: { message } }),
    );
  }
}

/// Push the current access token AND user id (if any) into the Rust
/// AppState. Called on app startup so an existing persisted session is
/// honoured without re-doing the OAuth dance, and again on every auth
/// state change emitted by supabase-js (refresh, sign-out, etc.).
///
/// `user_id` is the Supabase auth.users.id (uuid string). Rust uses it
/// to scope API key storage per account so signing in as user B never
/// exposes user A's BYOK Groq key.
export async function syncSessionToRust(): Promise<void> {
  const { data } = await supabase.auth.getSession();
  const token = data.session?.access_token;
  const userId = data.session?.user?.id;
  if (token && userId) {
    try {
      await invoke("set_session_token", { token, userId });
    } catch (e) {
      console.error("[auth] set_session_token failed:", e);
    }
  } else {
    try {
      await invoke("clear_session_token");
    } catch (e) {
      console.error("[auth] clear_session_token failed:", e);
    }
  }
}
