# Customer support runbook

Playbook for the production paywall. Most issues fall into one of seven
buckets — find the matching scenario and follow the steps.

**Tools you'll need open:**
- [Supabase Dashboard](https://supabase.com/dashboard/project/lefsnpgvlxcvozmnjsip) → SQL Editor + Edge Function Logs
- [Razorpay Dashboard](https://dashboard.razorpay.com) → Subscriptions + Payments
- Your support email

**First thing to do every time:** ask the customer for their **email**. Everything keys off that.

---

## Scenario 1 — "I paid but the app still says Free"

The most common production issue. Most likely cause: a webhook event got
lost in delivery.

### Step 1: ask the user to click Upgrade once more

The new `verify-subscription` self-heal runs on every Upgrade click —
in ~80% of cases this fixes itself without any admin action. If they
still see "Free" after clicking, proceed.

### Step 2: confirm payment landed in Razorpay

Razorpay dashboard → **Payments** → search by their email or amount.
Should see a ₹99 entry with status "captured" from today.

If you see the payment:
- It's a webhook delivery miss. Proceed to step 3.

If you DON'T see the payment:
- Customer was mistaken / paid the wrong thing / refunded.
- Ask them to share the Razorpay receipt email or transaction ID.

### Step 3: look up the subscription on Razorpay

Razorpay dashboard → **Subscriptions** → filter by customer email.
Find their `sub_xxx` ID and note its current status (Active / Paused / Halted).

### Step 4: manually sync via SQL

```sql
update public.profiles
   set plan_tier                = 'pro',
       subscription_status      = 'active',
       razorpay_subscription_id = 'sub_xxx_FROM_RAZORPAY',
       current_period_end       = 'YYYY-MM-DDT00:00:00Z',  -- 30 days from their payment
       paid_at                  = now(),
       updated_at               = now()
 where user_id = (select id from auth.users where email = 'customer@example.com');
```

### Step 5: tell the customer to close + reopen the app

The sidebar re-fetches the profile on mount. They should see "Pro · Renews ..." now.

---

## Scenario 2 — "My card was declined / payment failed"

Razorpay fires `subscription.halted` after 3 failed retries. Our webhook
catches this and downgrades the user to free immediately (no period
grace — they didn't successfully pay for it).

### Step 1: verify

```sql
select u.email, p.plan_tier, p.subscription_status, p.razorpay_subscription_id
  from public.profiles p
  join auth.users u on u.id = p.user_id
 where u.email = 'customer@example.com';
```

If `subscription_status='halted'` and `plan_tier='free_hosted'`, the
system already did the right thing.

### Step 2: tell the customer how to recover

"Click Upgrade for ₹99/mo again in the app — you'll go through a fresh
checkout where you can use a different payment method. Your previous
subscription has been ended."

The hardened `create-subscription` allows resubscribing because the old
sub is in a terminal state (`halted`).

---

## Scenario 3 — "I want a refund"

Razorpay handles the money side; you handle the access side.

### Step 1: refund in Razorpay

Razorpay dashboard → **Payments** → find their ₹99 charge → click ⋯ →
**Refund**. Choose full or partial. Razorpay returns the money to their
original payment method in 5-7 business days.

### Step 2: cancel their subscription

Razorpay dashboard → **Subscriptions** → find their `sub_xxx` → click ⋯ → **Cancel**.

### Step 3: revoke access in our DB

```sql
update public.profiles
   set plan_tier                = 'free_hosted',
       subscription_status      = 'cancelled',
       current_period_end       = null,
       razorpay_subscription_id = null,
       updated_at               = now()
 where user_id = (select id from auth.users where email = 'customer@example.com');
```

### Step 4: email the customer

"Refund of ₹99 has been processed. You'll see the credit on your card
in 5-7 business days. Your account has been downgraded to the free tier
(10 enhancements per day). Let us know if there's anything we can do
better next time."

---

## Scenario 4 — "I want to cancel but keep using until the period ends"

The standard self-serve cancellation flow handles this. The customer
clicks "Manage subscription" in the app, lands on Razorpay's portal,
clicks Cancel. Razorpay fires `subscription.cancelled`. Our webhook
keeps them on `plan_tier='pro'` until `current_period_end` passes
(because they paid for that window). After the period passes, the
`consume_daily_quota` RPC lazily downgrades them on the next enhance.

**Your job: nothing.** This works automatically.

If they email asking how to cancel, point them at the Manage subscription button.

---

## Scenario 5 — "I want to comp my friend / give a free month"

Two ways depending on duration:

**Forever (friend / team):**
```sql
update public.profiles
   set plan_tier  = 'unlimited',
       updated_at = now()
 where user_id = (select id from auth.users where email = 'friend@example.com');
```
Bypasses every gate, never paywalled.

**Specific number of days (promo / make-good):**
```sql
update public.profiles
   set plan_tier           = 'pro',
       subscription_status = 'active',
       current_period_end  = now() + interval '30 days',  -- adjust as needed
       paid_at             = now(),
       updated_at          = now()
 where user_id = (select id from auth.users where email = 'customer@example.com');
```
After 30 days they revert to free automatically (lazy downgrade).

---

## Scenario 6 — "I paid as one email, signed in as another"

Customer used Google account A to sign in but paid through Razorpay
with email B. The notes.user_id we stamp on the subscription points
to whichever account was signed in when they clicked Upgrade — that's
authoritative for our matchback.

### Step 1: figure out which account is signed-in-at-Upgrade-time

The Razorpay subscription's `notes.user_id` is the answer. Find the
subscription in Razorpay dashboard, click into it, look at "Notes" — it
shows our user UUID. Map UUID back to email:

```sql
select email from auth.users where id = 'UUID-FROM-RAZORPAY-NOTES';
```

### Step 2: tell the customer

"Your subscription is attached to account `<email-from-step-1>`. Please
sign into the app with that email to access Pro features."

If they want it on a DIFFERENT account, you can manually transfer it:

```sql
-- Move subscription metadata from account A to account B
update public.profiles a
   set razorpay_subscription_id = null,
       subscription_status      = null,
       current_period_end       = null,
       plan_tier                = 'free_hosted'
 where a.user_id = (select id from auth.users where email = 'WRONG-ACCOUNT@example.com');

update public.profiles
   set plan_tier                = 'pro',
       subscription_status      = 'active',
       razorpay_subscription_id = 'sub_xxx',
       current_period_end       = 'YYYY-MM-DDT00:00:00Z',
       paid_at                  = now(),
       updated_at               = now()
 where user_id = (select id from auth.users where email = 'CORRECT-ACCOUNT@example.com');
```

---

## Scenario 7 — Dispute / chargeback

The bank-initiated nuclear option. The customer disputes the charge with
their bank and Razorpay deducts the ₹99 back. They're effectively asking
for a refund without notifying you — sometimes legitimate (fraud), often
buyer's remorse.

### Step 1: respond to the dispute in Razorpay

Razorpay dashboard → **Disputes** → respond with evidence (the
subscription receipt, the customer's payment history, screenshots of
their app usage if any). You usually have 7-14 days.

### Step 2: revoke their access in our DB regardless of dispute outcome

```sql
update public.profiles
   set plan_tier                = 'free_hosted',
       subscription_status      = 'cancelled',
       current_period_end       = null,
       razorpay_subscription_id = null,
       updated_at               = now()
 where user_id = (select id from auth.users where email = 'disputing-customer@example.com');
```

Then cancel the underlying subscription in Razorpay so it doesn't keep
trying to charge.

### Step 3: ban them (optional)

For repeat dispute behaviour. Add their email to a denylist (not
implemented yet — for now manually flip them to `plan_tier='free_hosted'`
and add a note that they shouldn't be allowed back to pro).

---

## Quick reference

### Useful queries

```sql
-- Who's currently paying?
select u.email, p.current_period_end, p.subscription_status
  from public.profiles p
  join auth.users u on u.id = p.user_id
 where p.plan_tier = 'pro' and p.current_period_end > now()
 order by p.current_period_end;

-- Who's about to lapse this week?
select u.email, p.current_period_end, p.subscription_status
  from public.profiles p
  join auth.users u on u.id = p.user_id
 where p.plan_tier = 'pro'
   and p.current_period_end between now() and now() + interval '7 days';

-- Recent webhook errors (look in function logs for richer detail)
-- Supabase Dashboard → Edge Functions → razorpay-webhook → Logs

-- Today's signups
select email, created_at from auth.users
 where created_at >= current_date order by created_at desc;
```

### Function logs

Supabase Dashboard → Edge Functions → click the function name → Logs
tab. Filter by "Last 1 hour" / "Last 24 hours". The `[razorpay-webhook]`
prefix tags webhook events; search for it to see what fired.

### Manual verify-subscription trigger

If you want to force a re-sync from Razorpay for a specific user without
asking them to click Upgrade, you can call the function directly:

```powershell
# Get the user's JWT first (a bit fiddly — easier to just ask them to click Upgrade)
# Most of the time, manual SQL is faster than this.
```

The function is designed for the customer to call themselves via the app,
not for support-side invocation. If you find yourself wanting this often,
build a small admin dashboard instead.

---

## What's NOT in this runbook

These are real issues you might hit eventually but haven't designed for yet:
- **Failed renewal recovery flows** — if Razorpay auto-charges fail, customer keeps card-on-file dunning (email reminders to update). Build a "your card failed, update it" banner in the app once you have a few customers.
- **Bulk discounts / coupons** — Razorpay has Offers feature; not wired in.
- **Family / team plans** — not supported.
- **Annual billing option** — would need a second Razorpay Plan + a UI toggle.
- **Tax invoices for businesses** — GST collection; needed if you're charging Indian businesses.

Cross these bridges when at least 3 customers ask for the same thing.
