import { useEffect, useState } from "react";
import type { Session, User } from "@supabase/supabase-js";

import { isSupabaseConfigured, supabase } from "../lib/supabase";
import {
  installDeepLinkHandler,
  resetPasswordForEmail as resetPasswordForEmailImpl,
  signInWithGitHub as signInWithGitHubImpl,
  signInWithGoogle as signInWithGoogleImpl,
  signInWithPassword as signInWithPasswordImpl,
  signOut as signOutImpl,
  signUpWithPassword as signUpWithPasswordImpl,
  syncSessionToRust,
  updatePassword as updatePasswordImpl,
} from "../lib/auth";

export interface AuthState {
  /// True until the initial getSession() resolves.
  loading: boolean;
  configured: boolean;
  user: User | null;
  session: Session | null;
  signInWithGoogle: () => Promise<void>;
  signInWithGitHub: () => Promise<void>;
  signInWithPassword: (email: string, password: string) => Promise<void>;
  signUpWithPassword: (
    name: string,
    email: string,
    password: string,
  ) => Promise<{ needsEmailConfirmation: boolean }>;
  resetPasswordForEmail: (email: string) => Promise<void>;
  /// True when the user arrived via a password-reset email link. The
  /// session is established but the UI should force a "set a new
  /// password" step before falling through to the main app.
  recoveryMode: boolean;
  /// Set a new password and exit recovery mode. Session stays active.
  updatePassword: (newPassword: string) => Promise<void>;
  /// Exit recovery mode without changing the password. Signs out so
  /// the gate reappears.
  cancelRecovery: () => Promise<void>;
  /// True when the user *just* completed a fresh sign-in or sign-up
  /// (Supabase SIGNED_IN event). Stays false on app reload with a
  /// persisted session (INITIAL_SESSION event). Drives the
  /// PostAuthSetup interstitial.
  justSignedIn: boolean;
  /// Clear justSignedIn. Broadcasts to every useAuth instance so the
  /// App-level gate re-renders into Shell.
  dismissJustSignedIn: () => void;
  signOut: () => Promise<void>;
  /// Surface the last OAuth error (PKCE failure, provider rejection,
  /// network blip during exchangeCodeForSession). Cleared on the next
  /// successful sign-in attempt.
  error: string | null;
}

let deepLinkInstalled = false;

export function useAuth(): AuthState {
  const [session, setSession] = useState<Session | null>(null);
  const [loading, setLoading] = useState<boolean>(true);
  const [error, setError] = useState<string | null>(null);
  const [recoveryMode, setRecoveryMode] = useState<boolean>(false);
  const [justSignedIn, setJustSignedIn] = useState<boolean>(false);

  useEffect(() => {
    if (!isSupabaseConfigured) {
      setLoading(false);
      return;
    }

    let active = true;

    // Initial session load. Supabase reads from localStorage and
    // refreshes the token automatically if it has a refresh_token.
    supabase.auth
      .getSession()
      .then(({ data }) => {
        if (!active) return;
        setSession(data.session ?? null);
        // Push existing token into Rust on startup so the pipeline
        // can route to /enhance immediately without waiting for a
        // state change.
        return syncSessionToRust();
      })
      .catch((e) => {
        if (active) console.error("[useAuth] getSession failed:", e);
      })
      .finally(() => {
        if (active) setLoading(false);
      });

    const { data: sub } = supabase.auth.onAuthStateChange((event, newSession) => {
      setSession(newSession);
      if (event === "SIGNED_IN" || event === "TOKEN_REFRESHED") {
        setError(null);
      }
      // SIGNED_IN fires for fresh auth (OAuth callback, password
      // sign-in/sign-up). INITIAL_SESSION fires on app reload with a
      // persisted session — we deliberately ignore that one so
      // returning users don't see the post-auth interstitial.
      if (event === "SIGNED_IN") {
        window.dispatchEvent(new CustomEvent("pf-just-signed-in"));
      }
      if (event === "SIGNED_OUT") {
        window.dispatchEvent(new CustomEvent("pf-clear-just-signed-in"));
      }
      // Mirror the new auth state into Rust so the pipeline picks
      // it up on the next enhancement call.
      void syncSessionToRust();
    });

    // Deep link callback handler — installed once per window. Auth
    // callbacks arrive as perfectprompt://auth/callback?code=... in the
    // main window since it has the deep-link capability.
    if (!deepLinkInstalled) {
      deepLinkInstalled = true;
      void installDeepLinkHandler().catch((e) => {
        console.error("[useAuth] installDeepLinkHandler failed:", e);
      });
    }

    const onAuthError = (e: Event) => {
      const detail = (e as CustomEvent<{ message?: string }>).detail;
      if (detail?.message) setError(detail.message);
    };
    window.addEventListener("pf-auth-error", onAuthError);

    // Recovery-mode broadcast bridge. Multiple useAuth instances live
    // in the tree (App, Shell, Settings, etc.) — when one calls
    // updatePassword/cancelRecovery, every instance needs to flip its
    // local recoveryMode flag so MainAppGated re-renders and lets the
    // user through.
    const onRecoveryEnter = () => setRecoveryMode(true);
    const onRecoveryComplete = () => setRecoveryMode(false);
    window.addEventListener("pf-recovery-mode", onRecoveryEnter);
    window.addEventListener("pf-recovery-complete", onRecoveryComplete);

    // justSignedIn cross-instance bridge — every useAuth instance in
    // the tree flips together so App can show the interstitial,
    // PostAuthSetup can dismiss it, and Shell ends up rendered.
    const onJustSignedIn = () => setJustSignedIn(true);
    const onClearJustSignedIn = () => setJustSignedIn(false);
    window.addEventListener("pf-just-signed-in", onJustSignedIn);
    window.addEventListener("pf-clear-just-signed-in", onClearJustSignedIn);

    return () => {
      active = false;
      sub.subscription.unsubscribe();
      window.removeEventListener("pf-auth-error", onAuthError);
      window.removeEventListener("pf-recovery-mode", onRecoveryEnter);
      window.removeEventListener("pf-recovery-complete", onRecoveryComplete);
      window.removeEventListener("pf-just-signed-in", onJustSignedIn);
      window.removeEventListener("pf-clear-just-signed-in", onClearJustSignedIn);
    };
  }, []);

  return {
    loading,
    configured: isSupabaseConfigured,
    user: session?.user ?? null,
    session,
    error,
    signInWithGoogle: async () => {
      setError(null);
      try {
        await signInWithGoogleImpl();
      } catch (e) {
        const msg = (e as Error)?.message ?? String(e);
        setError(msg);
        throw e;
      }
    },
    signInWithGitHub: async () => {
      setError(null);
      try {
        await signInWithGitHubImpl();
      } catch (e) {
        const msg = (e as Error)?.message ?? String(e);
        setError(msg);
        throw e;
      }
    },
    signInWithPassword: async (email, password) => {
      setError(null);
      try {
        await signInWithPasswordImpl(email, password);
      } catch (e) {
        const msg = (e as Error)?.message ?? String(e);
        setError(msg);
        throw e;
      }
    },
    signUpWithPassword: async (name, email, password) => {
      setError(null);
      try {
        return await signUpWithPasswordImpl(name, email, password);
      } catch (e) {
        const msg = (e as Error)?.message ?? String(e);
        setError(msg);
        throw e;
      }
    },
    resetPasswordForEmail: async (email) => {
      setError(null);
      try {
        await resetPasswordForEmailImpl(email);
      } catch (e) {
        const msg = (e as Error)?.message ?? String(e);
        setError(msg);
        throw e;
      }
    },
    recoveryMode,
    updatePassword: async (newPassword) => {
      setError(null);
      try {
        await updatePasswordImpl(newPassword);
        window.dispatchEvent(new CustomEvent("pf-recovery-complete"));
      } catch (e) {
        const msg = (e as Error)?.message ?? String(e);
        setError(msg);
        throw e;
      }
    },
    cancelRecovery: async () => {
      try {
        await signOutImpl();
      } finally {
        window.dispatchEvent(new CustomEvent("pf-recovery-complete"));
      }
    },
    justSignedIn,
    dismissJustSignedIn: () => {
      window.dispatchEvent(new CustomEvent("pf-clear-just-signed-in"));
    },
    signOut: async () => {
      await signOutImpl();
    },
  };
}
