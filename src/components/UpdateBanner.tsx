import { openUrl } from "@tauri-apps/plugin-opener";
import "./UpdateBanner.css";

/// Direct .msi download URL. Goes through Vercel's /download redirect
/// which itself 308s to github.com/.../releases/latest/download/<file>,
/// so the browser immediately starts downloading the .msi without
/// dropping the user on the GitHub release page first. The redirect
/// is in docs/vercel.json and is updated every release.
const DOWNLOAD_URL = "https://perfectprompt-beta.vercel.app/download";

interface Props {
  /// The latest released version (e.g. "0.4.8"). Shown in the
  /// banner copy.
  version: string;
  /// GitHub release page URL — unused at click-time (we go direct
  /// to the .msi via Vercel) but kept on the prop in case future UI
  /// wants to expose a "See release notes" affordance.
  releaseUrl: string;
  /// Hide the banner for this version. Also fired when the user
  /// clicks Update now — once they've triggered the download,
  /// pinging them about the same version again would be noise.
  onDismiss: () => void;
}

export function UpdateBanner({ version, releaseUrl: _releaseUrl, onDismiss }: Props) {
  const handleUpdate = () => {
    openUrl(DOWNLOAD_URL).catch((e) =>
      console.error("[update-banner] openUrl failed:", e),
    );
    onDismiss();
  };

  return (
    <div className="pf-update-banner" role="alert" aria-live="polite">
      <button
        type="button"
        className="pf-update-banner-close"
        onClick={onDismiss}
        aria-label="Dismiss update notification"
      >
        ×
      </button>
      <div className="pf-update-banner-title">New update available</div>
      <div className="pf-update-banner-message">
        PerfectPrompt {version} is ready to install.
      </div>
      <div className="pf-update-banner-actions">
        <button
          type="button"
          className="pf-update-banner-primary"
          onClick={handleUpdate}
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
    </div>
  );
}
