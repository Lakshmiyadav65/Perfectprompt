# web/ — deployed website

This folder is the **public site deployed to Vercel** (`perfectprompt-beta.vercel.app`).
It is *not* documentation — see [../docs/](../docs/) for that.

> **Vercel config:** the project's **Root Directory** must be set to `web` in the
> Vercel dashboard. The files below are then served from the domain root, so the
> public URLs are unchanged by this folder's location:

| File | Served at | Used by |
|------|-----------|---------|
| `index.html` | `/` | marketing / download landing page |
| `auth-success.html` | `/auth-success.html` | OAuth redirect target — also in Supabase Auth "Redirect URLs" allowlist. Referenced by [`src/lib/supabase.ts`](../src/lib/supabase.ts). |
| `latest.json` | `/latest.json` | Tauri auto-updater manifest — endpoint hard-coded in [`src-tauri/tauri.conf.json`](../src-tauri/tauri.conf.json). |
| `vercel.json` | — | Vercel config (`cleanUrls`, the `/download` redirect, headers). |

**Changing any public path here means updating the matching hard-coded URL in the app.**
