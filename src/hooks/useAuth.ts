import { useEffect, useState } from "react";
import type { Session, User } from "@supabase/supabase-js";

import { isSupabaseConfigured, supabase } from "../lib/supabase";
import {
  installDeepLinkHandler,
  signInWithGitHub as signInWithGitHubImpl,
  signInWithGoogle as signInWithGoogleImpl,
  signOut as signOutImpl,
  syncSessionToRust,
} from "../lib/auth";

export interface AuthState {
  /// True until the initial getSession() resolves.
  loading: boolean;
  configured: boolean;
  user: User | null;
  session: Session | null;
  signInWithGoogle: () => Promise<void>;
  signInWithGitHub: () => Promise<void>;
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

    return () => {
      active = false;
      sub.subscription.unsubscribe();
      window.removeEventListener("pf-auth-error", onAuthError);
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
    signOut: async () => {
      await signOutImpl();
    },
  };
}
