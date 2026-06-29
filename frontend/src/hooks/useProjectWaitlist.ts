import { useCallback, useEffect, useState } from "react";

import { isSupabaseConfigured, supabase } from "../lib/supabase";

/// Drives the Projects beta-waitlist banner.
///   - "loading"  : checking whether the signed-in user already joined
///   - "idle"     : not on the list yet — show the CTA
///   - "joining"  : insert in flight
///   - "joined"   : on the list — show the confirmed state
///   - "error"    : the join failed; `error` holds a user-facing message
export type WaitlistStatus = "loading" | "idle" | "joining" | "joined" | "error";

export interface ProjectWaitlist {
  status: WaitlistStatus;
  error: string | null;
  join: () => Promise<void>;
}

/// One-click waitlist signup backed by the `project_waitlist` table
/// (migration 0005). The signed-in user's id + email are upserted in a
/// single click — no form, because every user is already authenticated.
///
/// The status check fails *open* to "idle": if the lookup errors (table
/// not deployed yet, transient network blip) we still let the user click
/// Join rather than hiding the CTA behind a spinner forever.
export function useProjectWaitlist(): ProjectWaitlist {
  const [status, setStatus] = useState<WaitlistStatus>("loading");
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let active = true;

    if (!isSupabaseConfigured) {
      setStatus("idle");
      return;
    }

    void (async () => {
      try {
        const {
          data: { session },
        } = await supabase.auth.getSession();
        const userId = session?.user?.id;
        if (!userId) {
          if (active) setStatus("idle");
          return;
        }
        const { data, error: selErr } = await supabase
          .from("project_waitlist")
          .select("user_id")
          .eq("user_id", userId)
          .maybeSingle();
        if (selErr) throw selErr;
        if (active) setStatus(data ? "joined" : "idle");
      } catch (e) {
        // Fail open — never trap the user behind a perpetual loading state.
        console.warn("[waitlist] status check failed, defaulting to idle:", e);
        if (active) setStatus("idle");
      }
    })();

    return () => {
      active = false;
    };
  }, []);

  const join = useCallback(async () => {
    setError(null);
    setStatus("joining");
    try {
      const {
        data: { session },
      } = await supabase.auth.getSession();
      const user = session?.user;
      if (!user) {
        throw new Error("You need to be signed in to join the waitlist.");
      }
      const { error: upErr } = await supabase
        .from("project_waitlist")
        .upsert(
          { user_id: user.id, email: user.email ?? null },
          { onConflict: "user_id", ignoreDuplicates: true },
        );
      if (upErr) throw upErr;
      setStatus("joined");
    } catch (e) {
      console.error("[waitlist] join failed:", e);
      setError(
        e instanceof Error
          ? e.message
          : "Couldn't join the waitlist. Please try again.",
      );
      setStatus("error");
    }
  }, []);

  return { status, error, join };
}
