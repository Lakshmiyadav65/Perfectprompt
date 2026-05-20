import { openUrl } from "@tauri-apps/plugin-opener";
import "./UpdateBanner.css";

interface Props {
  /// The latest released version (e.g. "0.4.6"). Shown in the
  /// banner copy.
  version: string;
  /// GitHub release page URL the "Update now" CTA opens. Auto-update
  /// (in-place download + install) isn't wired yet, so we link the
  /// user to the manual download instead.
  releaseUrl: string;
  /// Hide the banner for this version. Also fired when the user
  /// clicks Update now — once they've opened the release page,
  /// pinging them about the same version again would be noise.
  onDismiss: () => void;
}

export function UpdateBanner({ version, releaseUrl, onDismiss }: Props) {
  const handleUpdate = () => {
    openUrl(releaseUrl).catch((e) =>
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
