import { useCallback, useEffect, useState } from "react";
import type { User } from "@supabase/supabase-js";

/// Effective display name used in the sidebar profile chip.
///
/// Resolution order:
///   1. User-set override (localStorage `pf.display_name_override`) — wins
///      if set, lets the user pick whatever they want to see locally.
///   2. Google `full_name` from user_metadata (set by the OAuth provider).
///   3. The email's local-part (before `@`) — usable shorthand when no
///      name was supplied.
///   4. `"You"` — the historical fallback when signed out.
///
/// Cross-window sync via the `storage` event + a custom in-window event,
/// mirroring useEnhancementUsage so multiple PerfectPrompt windows stay
/// consistent without polling.

const STORAGE_KEY = "pf.display_name_override";
const UPDATE_EVENT = "pf-display-name-changed";

function loadOverride(): string {
  try {
    return localStorage.getItem(STORAGE_KEY) ?? "";
  } catch {
    return "";
  }
}

function persistOverride(value: string) {
  try {
    if (value.trim()) {
      localStorage.setItem(STORAGE_KEY, value.trim());
    } else {
      localStorage.removeItem(STORAGE_KEY);
    }
    window.dispatchEvent(new CustomEvent(UPDATE_EVENT));
  } catch {
    /* ignore */
  }
}

function nameFromUser(user: User | null): string {
  if (!user) return "";
  const meta = user.user_metadata ?? {};
  const fromMeta =
    (typeof meta.full_name === "string" && meta.full_name.trim()) ||
    (typeof meta.name === "string" && meta.name.trim()) ||
    "";
  if (fromMeta) return fromMeta;
  const email = user.email ?? "";
  if (email.includes("@")) return email.split("@")[0]!;
  return email;
}

export interface DisplayNameState {
  /// What to render in the chip.
  name: string;
  /// The user's current override (empty if none set).
  override: string;
  /// What the name would be without the override — surfaced so the
  /// Settings input can show "Currently: <Google name>" alongside.
  defaultName: string;
  setOverride: (next: string) => void;
}

export function useDisplayName(user: User | null): DisplayNameState {
  const [override, setOverrideState] = useState<string>(loadOverride);

  useEffect(() => {
    const refresh = () => setOverrideState(loadOverride());
    window.addEventListener(UPDATE_EVENT, refresh);
    window.addEventListener("storage", refresh);
    return () => {
      window.removeEventListener(UPDATE_EVENT, refresh);
      window.removeEventListener("storage", refresh);
    };
  }, []);

  const defaultName = nameFromUser(user) || "You";
  const name = (override.trim() || defaultName).trim();

  const setOverride = useCallback((next: string) => {
    persistOverride(next);
    setOverrideState(next.trim());
  }, []);

  return { name, override, defaultName, setOverride };
}
