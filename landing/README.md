# landing/ — deployed website

This folder is the **public site deployed to Vercel** (`perfectprompt-beta.vercel.app`).
It is *not* documentation — see [../docs/](../docs/) for that.

> **Vercel config:** the project's **Root Directory** must be set to `landing` in the
> Vercel dashboard (Project → Settings → Build & Deployment → Root Directory). The files
> below are then served from the domain root, so the public URLs are unchanged by this
> folder's location.
>
> ⚠️ This folder was renamed `web/` → `landing/` in the repo restructure. If deploys stop
> updating, it's almost certainly because the Vercel Root Directory still says `web` — set
> it to `landing` and redeploy.

| File | Served at | Used by |
|------|-----------|---------|
| `index.html` | `/` | marketing / download landing page |
| `auth-success.html` | `/auth-success.html` | OAuth redirect target — also in Supabase Auth "Redirect URLs" allowlist. Referenced by [`../frontend/src/lib/supabase.ts`](../frontend/src/lib/supabase.ts). |
| `latest.json` | `/latest.json` | Tauri auto-updater manifest — endpoint hard-coded in [`../backend/src-tauri/tauri.conf.json`](../backend/src-tauri/tauri.conf.json). |
| `vercel.json` | — | Vercel config (`cleanUrls`, the `/download` redirect, headers). |

**Changing any public path here means updating the matching hard-coded URL in the app.**
