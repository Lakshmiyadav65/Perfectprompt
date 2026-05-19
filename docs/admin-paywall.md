# Paywall admin guide

Quick reference for the manual flows around the lifetime free-trial paywall.
All actions run against the `public.profiles` table in your Supabase project —
**Dashboard → SQL Editor → New query → paste → Run**.

The schema this guide assumes is in [supabase/migrations/0002_paywall.sql](../supabase/migrations/0002_paywall.sql).

---

## Find a user by email

User-facing email lives on `auth.users`, the `profiles` table only carries
the foreign-key `user_id`. So every lookup starts here:

```sql
select u.id as user_id,
       u.email,
       p.plan_tier,
       p.lifetime_used,
       p.extra_granted,
       p.lifetime_used as used,
       (10 + p.extra_granted) as effective_limit,
       p.paid_at
  from auth.users u
  left join public.profiles p on p.user_id = u.id
 where u.email = 'customer@example.com';
```

Result tells you:
- `plan_tier = 'free_hosted'` and `used >= effective_limit` → blocked, will see Upgrade CTA
- `plan_tier = 'pro'` or `'unlimited'` → paid, no cap
- `extra_granted > 0` → admin grant active, "Special access" badge in sidebar

---

## Grant a customer +N extra enhancements

Use when someone needs a few more on the house — bug-bounty thank-you, a
demo lead asking for extra runs, etc. **Adds to the existing balance** so
running this twice grants twice as many.

```sql
-- Replace EMAIL and N
update public.profiles
   set extra_granted = extra_granted + N,
       updated_at    = now()
 where user_id = (select id from auth.users where email = 'EMAIL');
```

Examples (the exact `+N` values the user spec called out):

```sql
-- +2 enhancements
update public.profiles set extra_granted = extra_granted +  2, updated_at = now()
 where user_id = (select id from auth.users where email = 'customer@example.com');

-- +10 enhancements
update public.profiles set extra_granted = extra_granted + 10, updated_at = now()
 where user_id = (select id from auth.users where email = 'customer@example.com');

-- +20 enhancements
update public.profiles set extra_granted = extra_granted + 20, updated_at = now()
 where user_id = (select id from auth.users where email = 'customer@example.com');

-- +30 enhancements
update public.profiles set extra_granted = extra_granted + 30, updated_at = now()
 where user_id = (select id from auth.users where email = 'customer@example.com');
```

The user's sidebar updates the next time their app calls `/enhance` (which
re-reads the quota), or when they restart the app. No client deploy needed.

---

## Mark a user as paid (after Razorpay confirms ₹200)

Razorpay sends you an email when a payment lands. Once you've confirmed
the payer's app email matches, flip them to the paid tier:

```sql
update public.profiles
   set plan_tier = 'pro',
       paid_at   = now(),
       updated_at = now()
 where user_id = (select id from auth.users where email = 'customer@example.com');
```

`'pro'` and `'unlimited'` both bypass the cap; use `'unlimited'` only if
you've decided this user is permanently on the house (yourself, internal
team, etc.).

---

## Revoke paid access / undo a grant

Refund? Chargeback? Wrong user upgraded by mistake?

```sql
-- Revert to free tier (keeps lifetime_used so they don't suddenly get 10 fresh)
update public.profiles
   set plan_tier = 'free_hosted',
       paid_at   = null,
       updated_at = now()
 where user_id = (select id from auth.users where email = 'customer@example.com');

-- Remove an admin grant
update public.profiles
   set extra_granted = 0,
       updated_at = now()
 where user_id = (select id from auth.users where email = 'customer@example.com');

-- Subtract a specific number (won't go below zero)
update public.profiles
   set extra_granted = greatest(0, extra_granted - 5),
       updated_at = now()
 where user_id = (select id from auth.users where email = 'customer@example.com');
```

If the user is over the new limit after a revoke, the next `/enhance` call
will 429 and the sidebar's Upgrade CTA reappears.

---

## Reset a user's lifetime count

For testing only — don't do this for real customers without a reason.

```sql
update public.profiles
   set lifetime_used = 0,
       updated_at    = now()
 where user_id = (select id from auth.users where email = 'customer@example.com');
```

---

## Audit: who paid this month?

```sql
select u.email, p.plan_tier, p.paid_at, p.lifetime_used
  from public.profiles p
  join auth.users u on u.id = p.user_id
 where p.paid_at >= date_trunc('month', now())
 order by p.paid_at desc;
```

## Audit: who's about to hit the cap?

```sql
select u.email,
       p.lifetime_used,
       p.extra_granted,
       (10 + p.extra_granted) as effective_limit,
       (10 + p.extra_granted - p.lifetime_used) as remaining
  from public.profiles p
  join auth.users u on u.id = p.user_id
 where p.plan_tier = 'free_hosted'
   and (10 + p.extra_granted - p.lifetime_used) <= 2
 order by remaining asc;
```

Use this to nudge people who are one enhancement away from the wall — good
moment to message them with the Razorpay link directly.
