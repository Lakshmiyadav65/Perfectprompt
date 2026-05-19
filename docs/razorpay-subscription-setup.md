# Razorpay subscription setup

Replaces the static payment-link flow with proper recurring subscriptions.
After this is configured:

- Customer clicks **Upgrade for ₹99/mo** → `create-subscription` edge function calls Razorpay → returns a hosted checkout URL.
- Customer enters card / UPI mandate → Razorpay charges ₹99 immediately and every 30 days after.
- Razorpay POSTs `subscription.activated` → our webhook flips `plan_tier='pro'` and stamps `current_period_end`.
- On each renewal Razorpay POSTs `subscription.charged` → webhook extends `current_period_end` by another 30 days.
- Customer can revisit the subscription page (saved in `subscription_short_url`) to cancel any time → Razorpay POSTs `subscription.cancelled` → we keep them on pro until `current_period_end` passes.

---

## Prerequisites

- Razorpay merchant account **activated** (Live mode, KYC verified). Subscriptions don't work in Test mode for mandate registration; you need Live for real auto-debit.
- Razorpay account has **Subscriptions** enabled. Most do by default. If you don't see "Plans" or "Subscriptions" in the dashboard, contact Razorpay support to enable it.
- You've already done the previous Razorpay setup (Key ID, Key Secret, Webhook Secret all in Supabase). See [razorpay-webhook-setup.md](razorpay-webhook-setup.md).

---

## Step 1 — Create a Plan

A Plan is a reusable template ("₹99 every month"). One Plan can have many Subscriptions attached to it.

1. https://dashboard.razorpay.com → left sidebar **Subscriptions** → **Plans** → **Create Plan**
2. Fill in:

| Field | Value |
|---|---|
| **Plan Name** | `PerfectPrompt Monthly` |
| **Description** | `Unlimited prompt enhancements, ₹99/month` |
| **Period** | `Monthly` |
| **Interval** | `1` (every 1 month) |
| **Amount** | `99` (in rupees — Razorpay handles the paise math) |
| **Currency** | `INR` |
| **Notes** | optional; "v1 launch" or similar for your own records |

3. Click **Create Plan**. Razorpay generates a **Plan ID** like `plan_xxxxxxxxxxxxxx`.
4. **Copy the Plan ID** to your password manager — you'll set it as a Supabase secret next.

## Step 2 — Add `RAZORPAY_PLAN_ID` to Supabase secrets

In a terminal at `a:\Better---Prompt`:

```powershell
supabase secrets set RAZORPAY_PLAN_ID="plan_xxxxxxxxxxxxxx"
```

Verify:

```powershell
supabase secrets list
```

Should now show 4 Razorpay secrets total: `RAZORPAY_KEY_ID`, `RAZORPAY_KEY_SECRET`, `RAZORPAY_WEBHOOK_SECRET`, `RAZORPAY_PLAN_ID`.

## Step 3 — Add subscription events to the webhook

Razorpay's webhook (which you already created) is subscribed to `payment_link.paid` only. Add the subscription events:

1. Razorpay dashboard → **Account & Settings → Webhooks**
2. Click your existing PerfectPrompt webhook row → **Edit**
3. Under **Active Events**, scroll to the **Subscription** group and tick:
   - `subscription.activated`
   - `subscription.charged`
   - `subscription.cancelled`
   - `subscription.completed`
   - `subscription.halted`
   - `subscription.paused`
   - `subscription.resumed`
4. (Optional) Untick `payment_link.paid` — the static payment-link flow is being retired; the webhook now ignores those events anyway.
5. Click **Save** (or whatever the button says).

The webhook secret stays the same; you don't need to regenerate it.

## Step 4 — Apply the schema migration

```sql
-- Run in Supabase SQL Editor — paste the contents of
-- supabase/migrations/0003_subscription.sql
```

Verify the new columns landed:

```sql
select column_name
  from information_schema.columns
 where table_schema = 'public' and table_name = 'profiles'
   and column_name in ('razorpay_subscription_id', 'subscription_status', 'current_period_end', 'subscription_short_url')
 order by column_name;
```

Should return 4 rows.

## Step 5 — Deploy the three edge functions

```powershell
supabase functions deploy create-subscription
supabase functions deploy razorpay-webhook --no-verify-jwt
supabase functions deploy enhance
```

(`enhance` needs a redeploy because it now calls the new `consume_daily_quota` RPC instead of `consume_lifetime_quota`. `--no-verify-jwt` on `razorpay-webhook` stays the same as last time — Razorpay's servers can't include a Supabase JWT.)

## Step 6 — Disable the old static ₹199 payment link

So no one accidentally pays via the old flow:

1. Razorpay dashboard → **Payment Links**
2. Find your ₹199 PerfectPrompt link
3. Click the **⋯** menu → **Disable Link**

Anyone who tries to pay it now sees "This link is disabled". Refund any payments that come in via the old link manually (Razorpay → Payments → Refund).

## Step 7 — Grandfather your existing payer (Lakshmi)

If you already have customers from the old ₹199 lifetime model, set them
to `unlimited` so they're never paywalled again. From [admin-paywall.md](admin-paywall.md):

```sql
update public.profiles
   set plan_tier  = 'unlimited',
       updated_at = now()
 where user_id = (select id from auth.users where email = 'their-email@gmail.com');
```

## Step 8 — Test end-to-end

### 8a. Reset your test account

```sql
update public.profiles
   set plan_tier               = 'free_hosted',
       extra_granted           = 0,
       razorpay_subscription_id = null,
       subscription_status     = null,
       current_period_end      = null,
       subscription_short_url  = null,
       paid_at                 = null,
       updated_at              = now()
 where user_id = (select id from auth.users where email = 'YOUR-EMAIL@gmail.com');

delete from public.daily_usage
 where user_id = (select id from auth.users where email = 'YOUR-EMAIL@gmail.com');
```

### 8b. Hit the daily cap

Open the app. Enhance 10 times. Sidebar should switch to "Free 10 / 10 · Daily limit reached. Resets in Xh Ym" + an Upgrade button.

### 8c. Subscribe

Click **Upgrade for ₹99/mo**. A Razorpay subscription page should open. Pay with a real card / UPI (₹99 round-trips to your bank).

Within ~10 seconds, watch the function logs:
- Supabase dashboard → **Edge Functions → razorpay-webhook → Logs**
- Look for: `[razorpay-webhook] subscription.activated → user=<uuid> pro until <date>`

### 8d. Verify in DB

```sql
select plan_tier, subscription_status, current_period_end
  from public.profiles
 where user_id = (select id from auth.users where email = 'YOUR-EMAIL@gmail.com');
```

Should show `pro`, `active`, and `current_period_end` ~30 days out.

### 8e. Verify in the app

Close + reopen the app. Sidebar should now show "Pro · Renews <date>" with no daily counter, and the button reads "Manage subscription" (clicks back to the same Razorpay portal).

### 8f. (Optional) Cancel test

1. Click "Manage subscription" → cancel on Razorpay
2. Webhook fires `subscription.cancelled` → DB updates `subscription_status='cancelled'` but `current_period_end` stays
3. Sidebar keeps showing Pro until `current_period_end` passes
4. After that date (or you can manually set it to yesterday in SQL): sidebar flips to "Pro · lapsed" with a Resubscribe button

---

## Common snags

| Symptom | Cause | Fix |
|---|---|---|
| `Missing required env: RAZORPAY_PLAN_ID` in create-subscription logs | Step 2 not done | `supabase secrets set RAZORPAY_PLAN_ID="plan_xxx"` |
| Razorpay returns 400 on subscription create | Plan ID typo / wrong mode | Copy Plan ID from dashboard again. Make sure you're using Live mode keys with a Live mode Plan. |
| Webhook fires but plan_tier doesn't flip | `notes.user_id` missing | Means the subscription was created OUTSIDE create-subscription (e.g. manually in Razorpay dashboard). Such subscriptions can't auto-match. Run the manual SQL flip from admin-paywall.md. |
| Customer paid, subscription.activated never fires | Sometimes Razorpay takes 1-2 min on first auth. Wait. If still nothing after 5 min, check webhook event delivery in Razorpay dashboard → Webhooks → click your webhook → Recent Deliveries. |
| User stays on pro after canceling | By design — they paid for the current period, they get to use it. The lazy expiry in consume_daily_quota downgrades them on the next enhancement after `current_period_end`. |
| User canceled in Razorpay but DB still says active | Webhook didn't fire. Manually run the revoke SQL from admin-paywall.md. |

---

## What this REPLACES from the previous setup

Files now superseded but kept in the repo:
- `supabase/migrations/0002_paywall.sql` — lifetime ₹199 schema. New columns from 0003 sit alongside; the `lifetime_used`/`extra_granted` columns are reused (`extra_granted` now means "+N per day" not "+N lifetime").
- `docs/razorpay-webhook-setup.md` — original webhook setup. Steps 1-3 still apply (Key ID/Secret, Webhook Secret); step 4+ is replaced by this file.
- Old `create-payment-link` edge function — removed in favor of `create-subscription`.
