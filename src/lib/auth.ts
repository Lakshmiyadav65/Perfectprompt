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

export async function signOut(): Promise<void> {
  await supabase.auth.signOut();
  try {
    await invoke("clear_session_token");
  } catch (e) {
    console.error("[auth] clear_session_token failed:", e);
  }
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
    const { data, error } = await supabase.auth.exchangeCodeForSession(code);
    if (error) throw error;
    const token = data.session?.access_token;
    if (!token) throw new Error("exchangeCodeForSession returned no access_token");
    await invoke("set_session_token", { token });
  } catch (e) {
    const message = (e as Error)?.message ?? String(e);
    console.error("[auth] callback handling failed:", message);
    window.dispatchEvent(
      new CustomEvent("pf-auth-error", { detail: { message } }),
    );
  }
}

/// Push the current access token (if any) into the Rust AppState.
/// Called on app startup so an existing persisted session is honoured
/// without re-doing the OAuth dance, and again on every auth state
/// change emitted by supabase-js (refresh, sign-out, etc.).
export async function syncSessionToRust(): Promise<void> {
  const { data } = await supabase.auth.getSession();
  const token = data.session?.access_token;
  if (token) {
    try {
      await invoke("set_session_token", { token });
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
