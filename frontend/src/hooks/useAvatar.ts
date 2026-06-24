import { useCallback, useEffect, useState } from "react";

/// Profile avatar override stored as a base64 data URL in localStorage.
///
/// We store the image inline (data URL) instead of writing to the Tauri
/// app data dir because:
///   1. It mirrors useDisplayName's localStorage model — one persistence
///      story for both halves of the profile, no Rust glue needed.
///   2. After canvas-resize to 256x256 JPEG, the payload is ~25-40KB,
///      well under the per-origin 5MB localStorage budget.
///   3. Rendering a data URL is identical to rendering any other src —
///      no async filesystem read in the hot path of the sidebar chip.
///
/// Cross-window sync via the `storage` event + a custom in-window event,
/// mirroring useDisplayName so the chip in every PerfectPrompt window
/// stays consistent without polling.

const STORAGE_KEY = "pf.avatar_data_url";
const UPDATE_EVENT = "pf-avatar-changed";

function loadAvatar(): string {
  try {
    return localStorage.getItem(STORAGE_KEY) ?? "";
  } catch {
    return "";
  }
}

function persistAvatar(value: string) {
  try {
    if (value) {
      localStorage.setItem(STORAGE_KEY, value);
    } else {
      localStorage.removeItem(STORAGE_KEY);
    }
    window.dispatchEvent(new CustomEvent(UPDATE_EVENT));
  } catch {
    /* ignore — localStorage full or unavailable */
  }
}

export interface AvatarState {
  /// Empty string when no avatar is set. Consumers should treat empty
  /// as "fall back to the initial-letter avatar".
  dataUrl: string;
  setAvatar: (dataUrl: string) => void;
  clearAvatar: () => void;
}

export function useAvatar(): AvatarState {
  const [dataUrl, setDataUrl] = useState<string>(loadAvatar);

  useEffect(() => {
    const refresh = () => setDataUrl(loadAvatar());
    window.addEventListener(UPDATE_EVENT, refresh);
    window.addEventListener("storage", refresh);
    return () => {
      window.removeEventListener(UPDATE_EVENT, refresh);
      window.removeEventListener("storage", refresh);
    };
  }, []);

  const setAvatar = useCallback((next: string) => {
    persistAvatar(next);
    setDataUrl(next);
  }, []);

  const clearAvatar = useCallback(() => {
    persistAvatar("");
    setDataUrl("");
  }, []);

  return { dataUrl, setAvatar, clearAvatar };
}

/// Resize an image file to a square JPEG of the given max dimension,
/// returning a data URL. Used by the Profile section so we don't dump
/// a multi-megabyte phone photo into localStorage. Centers and crops
/// to a square so the result fits the round chip without distortion.
export async function fileToResizedDataUrl(
  file: File,
  maxSize: number = 256,
  quality: number = 0.85,
): Promise<string> {
  const objectUrl = URL.createObjectURL(file);
  try {
    const img = await new Promise<HTMLImageElement>((resolve, reject) => {
      const el = new Image();
      el.onload = () => resolve(el);
      el.onerror = () => reject(new Error("Couldn't decode image"));
      el.src = objectUrl;
    });

    const side = Math.min(img.width, img.height);
    const sx = (img.width - side) / 2;
    const sy = (img.height - side) / 2;

    const canvas = document.createElement("canvas");
    canvas.width = maxSize;
    canvas.height = maxSize;
    const ctx = canvas.getContext("2d");
    if (!ctx) throw new Error("Canvas 2D context unavailable");
    ctx.drawImage(img, sx, sy, side, side, 0, 0, maxSize, maxSize);

    return canvas.toDataURL("image/jpeg", quality);
  } finally {
    URL.revokeObjectURL(objectUrl);
  }
}
