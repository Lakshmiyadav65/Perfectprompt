# Backend setup (Supabase)

PromptForge's optional hosted tiers run on Supabase. This doc covers
the one-time setup: creating the project, applying the schema,
configuring auth, and deploying the `/enhance` edge function.

The desktop app continues to work without any of this — users on the
**BYOK** tier call Groq directly with their own key and never touch
the backend.

---

## Architecture at a glance

```
                 ┌──────────────────┐
                 │ Tauri client     │
                 │ (Rust + React)   │
                 └──────┬───────────┘
                        │
   ┌────────────────────┼──────────────────────┐
   │ BYOK (no account)  │ Hosted (signed in)   │
   ▼                    ▼                      │
 Groq API           Supabase /enhance ────► Groq API
                    ▲                          ▲
                    │ JWT (Google/GitHub)      │ HOSTED_GROQ_API_KEY
                    │                          │ (server-side secret)
                    │ atomic consume_quota()   │
                    ▼                          │
                Postgres: profiles,            │
                  enhancements, daily_usage,   │
                  subscriptions                │
```

**Three tiers:**
- `BYOK` — no account, user's own Groq key, no quota.
- `free_hosted` — account required, **50 enhancements/day**, our Groq key.
- `pro` — paid (Stripe; schema-ready, not wired), 1000/day default.

Daily quota resets at **00:00 UTC**.

---

## What you need to do once

### 1. Sign up + create the project

- Go to [supabase.com](https://supabase.com) and sign up (free tier).
- Click **New Project**. Pick a region close to your users (Mumbai = `ap-south-1` is closest from India).
- Choose a strong DB password and **save it in your password manager** — Supabase only shows it once.

### 2. Grab the credentials

In the dashboard, go to **Project Settings → API**. Copy:

| Field | Where it lives |
|---|---|
| Project URL (`https://<id>.supabase.co`) | `.env` as `SUPABASE_URL` |
| `anon` public key | `.env` as `SUPABASE_ANON_KEY` (safe to ship in the client) |
| `service_role` secret key | **Never in `.env` or the client.** Lives only in the function's secret store (see step 5). |

### 3. Install the Supabase CLI

```sh
npm install -g supabase
supabase login          # opens your browser
supabase link --project-ref <your-project-id>
```

The project ID is the subdomain of your Project URL (e.g., `abcdefghijkl` for `https://abcdefghijkl.supabase.co`).

### 4. Apply the schema

The migration creates the four tables, the auto-profile trigger, the
atomic `consume_quota()` RPC, and the RLS policies.

```sh
supabase db push
```

You should see `0001_initial_schema.sql` applied. Open the SQL editor in
the dashboard and run `select * from public.profiles limit 1;` to confirm.

### 5. Set the edge function secrets

The `/enhance` function needs your **hosted-tier Groq key** (separate from your dev `.env` one, so you can rotate them independently):

```sh
supabase secrets set HOSTED_GROQ_API_KEY=gsk_your_hosted_key
```

`SUPABASE_URL` and `SUPABASE_SERVICE_ROLE_KEY` are auto-injected by the Supabase runtime — you do not set them yourself.

### 6. Deploy the function

The function reads its system prompts from `_prompts.json`, which is
generated from `prompts/*.md`. Always run the sync before deploying:

```sh
npm run sync-prompts
supabase functions deploy enhance
```

To verify it's live:

```sh
curl -X POST https://<your-project-id>.supabase.co/functions/v1/enhance \
  -H "Authorization: Bearer <a-real-user-jwt>" \
  -H "Content-Type: application/json" \
  -d '{"input_text":"fix the dashboard","route":"code"}'
```

Without a JWT you should get `401 unauthenticated`. With a fresh
testing user's JWT you should get `200` and an `enhanced_text` field.

### 7. Configure OAuth providers

In the dashboard: **Authentication → Providers**.

**Google:**
1. Create a Google Cloud project (or reuse one).
2. **APIs & Services → Credentials → Create credentials → OAuth client ID** → **Web application**.
3. Authorized redirect URI: `https://<your-project-id>.supabase.co/auth/v1/callback`.
4. Paste client ID + secret into the Supabase dashboard's Google provider.

**GitHub:**
1. GitHub → **Settings → Developer settings → OAuth Apps → New OAuth App**.
2. Authorization callback URL: `https://<your-project-id>.supabase.co/auth/v1/callback`.
3. Paste client ID + secret into the Supabase dashboard's GitHub provider.

**Redirect URLs (Tauri deep link):**
In **Authentication → URL Configuration**, add `promptforge://auth/callback` to the **Redirect URLs** allowlist. The Tauri client will register this custom URL scheme so the OAuth flow can hand control back to the desktop app after sign-in.

---

## Quota policy details

`consume_quota(user_id)` performs a single atomic SQL operation:

```sql
insert into daily_usage (user_id, usage_date, count)
values ($1, today_utc, 1)
on conflict (user_id, usage_date) do update
  set count = daily_usage.count + 1
  where daily_usage.count < effective_limit
returning daily_usage.count
```

If `RETURNING` yields a row, the request is allowed. If it returns nothing, the row already existed and was over cap → denied.

This is race-safe: two concurrent hotkey presses contend on the
(user_id, usage_date) tuple, so at most one can land the final
allowed increment when the cap is reached.

**Failed Groq calls still consume one quota unit.** This is a
deliberate v1 choice for abuse protection. If upstream failures end
up biting real users, we can add a `refund_quota(user_id)` RPC that
the function calls on `502`. Don't do this preemptively.

---

## Updating prompts

Source of truth lives in `/prompts/*.md`. The flow is:

1. Edit the relevant `prompts/*.md` file.
2. `npm run sync-prompts`     — regenerates `supabase/functions/enhance/_prompts.json`.
3. `supabase functions deploy enhance` — ships the new prompts.

`_prompts.json` IS checked into git so deploys are reproducible. Don't edit it by hand — your changes will be overwritten by the next sync.

---

## What stays free, and when you'd pay

Free-tier limits that matter for this app:

| Resource | Free | Estimated PromptForge ceiling |
|---|---|---|
| DB | 500 MB | ~2.5M enhancement rows. Years, with monthly pruning. |
| MAU | 50,000 | Not the blocker. |
| Bandwidth | 5 GB / mo | Plenty for text payloads. |
| Edge function invocations | **500K / mo** | ~1.5K active users at 10 calls/day each. **Tightest ceiling.** |

When you cross either 500 MB DB or 500K function calls/month, the
next step is **$25/mo Supabase Pro**. This should arrive roughly
when Pro-tier subscription revenue starts covering it.

**Project pause:** free projects pause after 7 days of zero
activity. Once you have any DAU this never triggers. Pre-launch
mitigation: GitHub Actions cron pinging `/rest/v1/` every 6 hours.

---

## Database hygiene (deferred but worth knowing)

Once `enhancements` grows past ~100K rows, add a monthly cron in
the dashboard (**Database → Cron Jobs**):

```sql
delete from public.enhancements
where created_at < now() - interval '90 days';
```

The aggregate counts in `daily_usage` are tiny and don't need pruning.
