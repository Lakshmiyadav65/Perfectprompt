import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

/// Daily-usage tracker for the enhancement action.
///
/// Two sources, in order of precedence:
///   1. Server-backed: when the hosted-tier pipeline emits a
///      `hosted:quota` event (Rust → JS, fired after every signed-in
///      /enhance call), we snapshot the server's count and ignore the
///      local path. This is authoritative — consume_quota() in
///      Postgres is the source of truth.
///   2. Local fallback: BYOK path or signed-out users tick a counter
///      owned by the Rust pipeline. Rust is the source of truth because
///      it's the single success point every entry path funnels through
///      (silent hotkey from VS Code/IDEs, clarify popup, question card,
///      main app) — the JS event-listener approach the hook previously
///      used could miss enhancements when no webview was awake to
///      receive the event. localStorage is still mirrored so the
///      sidebar paints the right number instantly on launch before the
///      `get_usage_state` round-trip completes.

const STORAGE_KEY = "pf.enhancements.usage";
const UPDATE_EVENT = "pf-usage-changed";

export const DAILY_LIMIT = 50;

interface HostedQuota {
  used: number;
  limit: number;
  remaining: number;
  plan_tier: string;
  resets_at?: string | null;
}

interface UsageSnapshot {
  date: string; // YYYY-MM-DD (local, per Rust's Local::now())
  used: number;
  limit: number;
  remaining: number;
  limit_reached: boolean;
}

interface StoredUsage {
  date: string;
  count: number;
}

function todayKey(): string {
  const d = new Date();
  const y = d.getFullYear();
  const m = String(d.getMonth() + 1).padStart(2, "0");
  const day = String(d.getDate()).padStart(2, "0");
  return `${y}-${m}-${day}`;
}

function loadCachedCount(): number {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return 0;
    const parsed: StoredUsage = JSON.parse(raw);
    if (parsed.date !== todayKey()) return 0;
    const n = Number(parsed.count);
    return Number.isFinite(n) && n > 0 ? n : 0;
  } catch {
    return 0;
  }
}

function saveCachedCount(date: string, count: number) {
  try {
    const data: StoredUsage = { date, count };
    localStorage.setItem(STORAGE_KEY, JSON.stringify(data));
  } catch {
    /* localStorage unavailable or quota-exceeded — drop silently */
  }
}

export function useEnhancementUsage() {
  // Seed from the localStorage mirror so the sidebar paints the right
  // number on first render. The Rust round-trip below overwrites it
  // with the authoritative value within a tick.
  const [used, setUsed] = useState<number>(loadCachedCount);
  const [hosted, setHosted] = useState<HostedQuota | null>(null);

  useEffect(() => {
    const refresh = () => setUsed(loadCachedCount());
    window.addEventListener(UPDATE_EVENT, refresh);
    // `storage` fires in other windows when localStorage mutates —
    // keeps the sidebar count in sync if a popup window's cache
    // update lands first.
    window.addEventListener("storage", refresh);
    return () => {
      window.removeEventListener(UPDATE_EVENT, refresh);
      window.removeEventListener("storage", refresh);
    };
  }, []);

  useEffect(() => {
    // `alive` closes the StrictMode listen-race: in dev, effects run
    // twice on mount, so if the listen() promise resolves AFTER the
    // simulated unmount, the unlisten function leaks and the next
    // mount registers a second active listener for the same event.
    let alive = true;
    let unlistenHosted: (() => void) | undefined;
    let unlistenUsage: (() => void) | undefined;

    // Seed from Rust — the canonical count. Survives webview restarts,
    // applies the daily reset on the user's local date, and covers
    // enhancements that happened while no webview was open to receive
    // the live event.
    void invoke<UsageSnapshot>("get_usage_state")
      .then((snap) => {
        if (!alive) return;
        setUsed(snap.used);
        saveCachedCount(snap.date, snap.used);
        window.dispatchEvent(new CustomEvent(UPDATE_EVENT));
      })
      .catch((e) => {
        console.error("[useEnhancementUsage] get_usage_state failed:", e);
      });

    void listen<HostedQuota>("hosted:quota", (event) => {
      if (!alive) return;
      setHosted(event.payload);
    }).then((u) => {
      if (!alive) {
        u();
        return;
      }
      unlistenHosted = u;
    });

    // `usage:changed` is emitted by the Rust pipeline after every
    // successful enhancement (silent hotkey from any IDE/app, clarify
    // popup, question card, main app). The payload carries the new
    // count from the canonical Rust state, so multiple windows
    // listening simultaneously all converge to the same number without
    // racing or double-incrementing.
    void listen<UsageSnapshot>("usage:changed", (event) => {
      if (!alive) return;
      const snap = event.payload;
      setUsed(snap.used);
      saveCachedCount(snap.date, snap.used);
      window.dispatchEvent(new CustomEvent(UPDATE_EVENT));
    }).then((u) => {
      if (!alive) {
        u();
        return;
      }
      unlistenUsage = u;
    });

    return () => {
      alive = false;
      unlistenHosted?.();
      unlistenUsage?.();
    };
  }, []);

  const increment = useCallback(() => {
    // Retained for compatibility — call sites that previously bumped
    // the count from the UI now no-op here. The Rust pipeline is the
    // single source of truth; calling this would have desynchronised
    // the localStorage mirror from the Rust state. Left as an exported
    // surface so the hook contract stays stable for any external
    // caller.
  }, []);

  // Server data wins when present — it's authoritative for signed-in
  // users. Falls back to the Rust-owned local counter for BYOK /
  // signed-out users.
  const effectiveUsed = hosted ? hosted.used : used;
  const effectiveLimit = hosted ? hosted.limit : DAILY_LIMIT;

  return {
    used: effectiveUsed,
    limit: effectiveLimit,
    remaining: Math.max(effectiveLimit - effectiveUsed, 0),
    limitReached: effectiveUsed >= effectiveLimit,
    source: hosted ? ("hosted" as const) : ("local" as const),
    increment,
  };
}
