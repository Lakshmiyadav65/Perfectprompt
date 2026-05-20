import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

/// Background update-availability check. Runs once at mount + every
/// 6 hours while the app is open. Surfaces a dismissible banner in
/// the main Shell when a newer GitHub release exists than the locally
/// installed version. Per-version localStorage dismissal — clicking
/// Later or X on the banner suppresses ONLY that version, so the next
/// release re-shows it.
///
/// Source of truth: the `check_for_updates` Tauri command, which hits
/// GitHub's `/releases/latest` endpoint server-side. Pre-releases and
/// drafts are filtered out, so a real `gh release create` (or
/// equivalent) is the only thing that triggers a user notification —
/// never a code push to main.

interface UpdateInfo {
  current_version: string;
  latest_version: string;
  update_available: boolean;
  release_url: string;
  release_notes: string | null;
}

const RECHECK_INTERVAL_MS = 6 * 60 * 60 * 1000; // 6 hours
const DISMISSED_STORAGE_KEY = "pf.update.dismissedVersion";

export interface UpdateState {
  /// The full UpdateInfo from the Rust check, or null when no check
  /// has completed yet OR no update is available. The Updates section
  /// in Settings shouldn't read this for its "no updates" copy —
  /// that's the manual-button path.
  updateInfo: UpdateInfo | null;
  /// True when the banner should render. False when: no check has
  /// completed yet, no update available, or the user has dismissed
  /// the current latest version.
  showBanner: boolean;
  /// Mark the current latest_version as dismissed and hide the banner
  /// for the rest of the session (and across launches via
  /// localStorage). Newer versions re-show.
  dismiss: () => void;
}

function loadDismissed(): string | null {
  try {
    return localStorage.getItem(DISMISSED_STORAGE_KEY);
  } catch {
    return null;
  }
}

function saveDismissed(version: string): void {
  try {
    localStorage.setItem(DISMISSED_STORAGE_KEY, version);
  } catch {
    // localStorage unavailable (private mode, quota) — silent. Worst
    // case the user sees the banner once more next launch.
  }
}

export function useUpdateCheck(): UpdateState {
  const [updateInfo, setUpdateInfo] = useState<UpdateInfo | null>(null);
  const [dismissedVersion, setDismissedVersion] = useState<string | null>(
    loadDismissed,
  );

  useEffect(() => {
    let alive = true;

    const runCheck = async () => {
      try {
        const info = await invoke<UpdateInfo>("check_for_updates");
        if (!alive) return;
        if (info.update_available) {
          setUpdateInfo(info);
        } else {
          // Latest version locally — no banner. Don't blow away
          // previously-set state in case the next check fails; the
          // user is on the latest, the absence of a banner is correct.
          setUpdateInfo(null);
        }
      } catch (e) {
        // Auto-check failures are silent. The user can manually
        // retry from Settings → Updates if curious. Don't spam the
        // console either — just one debug-level log.
        console.debug("[useUpdateCheck] check skipped:", e);
      }
    };

    void runCheck();
    const interval = window.setInterval(() => {
      void runCheck();
    }, RECHECK_INTERVAL_MS);

    return () => {
      alive = false;
      window.clearInterval(interval);
    };
  }, []);

  const dismiss = () => {
    if (!updateInfo) return;
    setDismissedVersion(updateInfo.latest_version);
    saveDismissed(updateInfo.latest_version);
  };

  const showBanner =
    updateInfo !== null &&
    updateInfo.update_available &&
    updateInfo.latest_version !== dismissedVersion;

  return { updateInfo, showBanner, dismiss };
}
