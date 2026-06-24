import { useState } from "react";
import { check } from "@tauri-apps/plugin-updater";
import { relaunch } from "@tauri-apps/plugin-process";
import "./UpdateBanner.css";

/// Bottom-right update toast. The version + releaseUrl props come from
/// useUpdateCheck (which polls our own check_for_updates command for
/// the awareness check). When the user clicks Update now we hand off
/// to tauri-plugin-updater's `check()` → `downloadAndInstall()` →
/// `relaunch()` flow so the app updates itself in place — no manual
/// installer wizard, no user navigating to a download link.
///
/// The two checks (useUpdateCheck for awareness, plugin-updater for
/// install) are intentionally separate:
///   - useUpdateCheck reads GitHub's /releases/latest API, which is
///     what we publish to and is always up-to-date the moment we cut
///     a release. Cheap, no signature needed.
///   - plugin-updater hits our signed latest.json manifest on Vercel.
///     Only used when the user has already chosen to install — by
///     that point we've decided to spend the bandwidth on the actual
///     download and require the manifest's signature for tamper
///     resistance.

interface Props {
  version: string;
  /// Kept on the prop for backwards compat with the hook contract.
  /// Not used at click-time anymore — the updater plugin handles the
  /// download URL via the latest.json manifest it fetches from the
  /// updater endpoint configured in tauri.conf.json.
  releaseUrl: string;
  onDismiss: () => void;
}

type InstallState = "idle" | "downloading" | "ready" | "error";

export function UpdateBanner({ version, releaseUrl: _releaseUrl, onDismiss }: Props) {
  const [state, setState] = useState<InstallState>("idle");
  const [progress, setProgress] = useState<number>(0);
  const [errorMsg, setErrorMsg] = useState<string | null>(null);

  const handleUpdate = async () => {
    setState("downloading");
    setErrorMsg(null);
    setProgress(0);

    try {
      const update = await check();
      if (!update) {
        // The plugin disagrees with the GitHub check — maybe the
        // manifest hasn't refreshed yet on Vercel's edge. Bail with a
        // soft error; the user can retry in a moment.
        setState("error");
        setErrorMsg("Update not ready on our servers yet. Try again in a minute.");
        return;
      }

      // The plugin streams progress events as the .msi downloads.
      // We track total bytes received vs the contentLength reported
      // on the first event to drive the progress bar.
      let downloaded = 0;
      let total = 0;
      await update.downloadAndInstall((event) => {
        switch (event.event) {
          case "Started":
            total = event.data.contentLength ?? 0;
            break;
          case "Progress":
            downloaded += event.data.chunkLength;
            if (total > 0) {
              setProgress(Math.min(100, Math.round((downloaded / total) * 100)));
            }
            break;
          case "Finished":
            setProgress(100);
            setState("ready");
            break;
        }
      });

      // downloadAndInstall returns after the installer has been
      // launched. On Windows with installMode=passive, the installer
      // runs without UI; we restart the app to finish swapping the
      // binary. relaunch() exits cleanly + spawns the new exe.
      await relaunch();
    } catch (e) {
      console.error("[update-banner] update failed:", e);
      setState("error");
      setErrorMsg(
        e instanceof Error ? e.message : "Update failed. Please try again.",
      );
    }
  };

  return (
    <div className="pf-update-banner" role="alert" aria-live="polite">
      <button
        type="button"
        className="pf-update-banner-close"
        onClick={onDismiss}
        aria-label="Dismiss update notification"
        disabled={state === "downloading" || state === "ready"}
      >
        ×
      </button>

      {state === "idle" && (
        <>
          <div className="pf-update-banner-title">New update available</div>
          <div className="pf-update-banner-message">
            PerfectPrompt {version} is ready to install.
          </div>
          <div className="pf-update-banner-actions">
            <button
              type="button"
              className="pf-update-banner-primary"
              onClick={() => void handleUpdate()}
            >
              Update now
            </button>
            <button
              type="button"
              className="pf-update-banner-secondary"
              onClick={onDismiss}
            >
              Later
            </button>
          </div>
        </>
      )}

      {state === "downloading" && (
        <>
          <div className="pf-update-banner-title">Downloading {version}…</div>
          <div className="pf-update-banner-message">
            {progress > 0 ? `${progress}%` : "Starting…"}
          </div>
          <div className="pf-update-banner-progress" aria-hidden="true">
            <div
              className="pf-update-banner-progress-fill"
              style={{ width: `${progress}%` }}
            />
          </div>
        </>
      )}

      {state === "ready" && (
        <>
          <div className="pf-update-banner-title">Update ready</div>
          <div className="pf-update-banner-message">
            Restarting PerfectPrompt to finish…
          </div>
        </>
      )}

      {state === "error" && (
        <>
          <div className="pf-update-banner-title">Update failed</div>
          <div className="pf-update-banner-message">
            {errorMsg ?? "Couldn't install the update. You can try again."}
          </div>
          <div className="pf-update-banner-actions">
            <button
              type="button"
              className="pf-update-banner-primary"
              onClick={() => {
                setState("idle");
                setErrorMsg(null);
              }}
            >
              Try again
            </button>
            <button
              type="button"
              className="pf-update-banner-secondary"
              onClick={onDismiss}
            >
              Dismiss
            </button>
          </div>
        </>
      )}
    </div>
  );
}
