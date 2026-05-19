# Razorpay webhook setup

End goal: kill the manual `UPDATE plan_tier='pro'` SQL step. After this
runs, every successful ₹199 payment auto-flips the buyer to a paid plan
within a couple of seconds.

The architecture:

```
[App: Upgrade button click]
        ↓
[POST /functions/v1/create-payment-link  (JWT-authed)]
        ↓
[Edge function calls Razorpay API → creates link with notes.user_id]
        ↓
[Returns short_url → app opens it]
        ↓
[Customer pays ₹199]
        ↓
[Razorpay POSTs payment_link.paid → /functions/v1/razorpay-webhook]
        ↓
[Webhook verifies HMAC, reads notes.user_id, UPDATE profiles → 'pro']
        ↓
[Customer's app refreshes → "Paid access · Unlimited"]
```

Three things to do in order. Don't skip steps — the webhook fails silently
if any secret is wrong, and you'll only notice when a real customer pays
and isn't unlocked.

---

## 1. Get Razorpay API credentials (~3 min)

These let our edge function call Razorpay on your behalf to create
payment links.

1. Open https://dashboard.razorpay.com → top-right account icon → **Account & Settings**
2. Left sidebar → **Website and app settings** → **API Keys**
3. Click **Generate Key** (or **Regenerate Live Key** if you already have one and have a copy saved)
4. **Copy both values immediately** — the dashboard only shows the
   secret once, you can't read it back later:
   - **Key Id** (starts with `rzp_live_...` for production or `rzp_test_...` for test mode)
   - **Key Secret** (long random string)
5. Stash them in your password manager. You'll paste them in step 3.

## 2. Create the webhook endpoint (~3 min)

This is what tells Razorpay "POST every payment event to my server".

1. Same dashboard → left sidebar **Account & Settings** → **Webhooks**
2. Click **Add New Webhook**
3. **Webhook URL**: `https://<your-supabase-ref>.supabase.co/functions/v1/razorpay-webhook`
   - Find `<your-supabase-ref>` in your Supabase dashboard URL, e.g. `lefsnpgvlxcvozmnjsip` for this project. Full URL becomes `https://lefsnpgvlxcvozmnjsip.supabase.co/functions/v1/razorpay-webhook`.
4. **Secret**: type a long random string (use a password manager to
   generate, e.g. 32+ chars). **Copy this** — you'll paste it as
   `RAZORPAY_WEBHOOK_SECRET` in step 3. Razorpay never shows it again.
5. **Alert Email**: your support email — Razorpay emails you when the
   webhook fails 3+ times.
6. **Active Events**: tick **only** `payment_link.paid`. (Don't enable
   payment.captured / payment.failed unless you want to handle them
   separately; our function 200-OKs other events but it's cleaner not
   to receive them.)
7. Click **Create Webhook**.

## 3. Set the three secrets in Supabase (~2 min)

The edge functions read these at runtime. They are NEVER committed to git.

```powershell
# From the repo root, with supabase CLI logged in & project linked:
supabase secrets set RAZORPAY_KEY_ID="rzp_live_xxxxxxxxxxxxxx"
supabase secrets set RAZORPAY_KEY_SECRET="xxxxxxxxxxxxxxxxxxxxxxxx"
supabase secrets set RAZORPAY_WEBHOOK_SECRET="the-32-char-string-from-step-2"
```

Or via dashboard: **Project Settings → Edge Functions → Manage Secrets → Add new secret** for each of the three.

Verify they landed:

```powershell
supabase secrets list
```

You should see all three names (values won't print — that's correct).

## 4. Deploy the two edge functions (~1 min)

```powershell
supabase functions deploy create-payment-link
supabase functions deploy razorpay-webhook
```

After deploy, hit them once each from the dashboard or curl to confirm
they boot (don't worry about the response):

```powershell
# Should return 401 — that's the auth check working
curl -X POST https://<your-ref>.supabase.co/functions/v1/create-payment-link

# Should return 401 — that's the signature check working
curl -X POST https://<your-ref>.supabase.co/functions/v1/razorpay-webhook
```

If either returns a 500 with `Missing required env`, the secrets aren't
set correctly. Re-run step 3.

## 5. Test it end-to-end (~5 min)

1. In Supabase SQL Editor, reset your test account back to free:
   ```sql
   update public.profiles
      set plan_tier     = 'free_hosted',
          lifetime_used = 10,
          paid_at       = null,
          updated_at    = now()
    where user_id = (select id from auth.users where email = 'YOUR-EMAIL@gmail.com');
   ```
2. Reopen the app. Sidebar should show "Free trial 10 / 10 · Upgrade for ₹199".
3. Click **Upgrade for ₹199**. A Razorpay payment page should open in
   your browser. **Verify the URL** — it should be a fresh
   `rzp.io/i/...` or `razorpay.com/payment-link/...`, NOT the static
   one you used before. If it's the static one, the function call
   failed and we silently fell back.
4. Complete the payment with a real card / UPI (it's your own money,
   ₹199 round-trip into your bank).
5. Within ~5 seconds of payment, Razorpay POSTs the webhook → function
   runs → DB updates.
6. **Don't close the app yet** — verify the flip landed:
   ```sql
   select plan_tier, paid_at from public.profiles
    where user_id = (select id from auth.users where email = 'YOUR-EMAIL@gmail.com');
   ```
   Should show `plan_tier = 'pro'`, `paid_at` within the last minute.
7. Close + reopen the app. Sidebar shows "Paid access · Unlimited enhancements".

## 6. Watch the function logs for the first few real customers

Supabase dashboard → **Edge Functions → razorpay-webhook → Logs**.

Look for: `[razorpay-webhook] flipped user=<uuid> to pro (payment_link=plink_xxx)`

If you see `signature mismatch` → your `RAZORPAY_WEBHOOK_SECRET` doesn't match what's in the Razorpay dashboard. Regenerate the webhook secret on both sides.

If you see `missing notes.user_id` → someone paid your *old* static
payment link (the `rzp.io/rzp/...` URL still in `.env`). You'll have
to match by email and flip manually for them. Solution: once the new
flow is verified working, delete the static payment link from Razorpay
and remove `VITE_RAZORPAY_LINK` from `.env`.

---

## Why both `create-payment-link` and `razorpay-webhook`?

The naive setup (single static Razorpay link, webhook matches by email)
fails ~10% of the time — customers sign up with one email and pay with
another, Gmail aliases vs work emails, typos at checkout. Per-user
links pass the `notes.user_id` UUID through Razorpay's system as a
correlation token, which makes matching deterministic.

The webhook is still mandatory even with per-user links — without it
the link gets created but no one tells Supabase the payment landed.
