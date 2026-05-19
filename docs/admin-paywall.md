# Paywall admin guide

Quick reference for the manual SQL operations around the current paywall.
All snippets run in **Supabase Dashboard → SQL Editor → New query**.

**Current model** (set 2026-05-19, second revision today): daily-free + monthly-pro subscription.

- **`free_hosted`** users: 10 enhancements per IST day (+ `extra_granted` bonus). Capped daily.
- **`pro`** users: unlimited, but only while `current_period_end > now()`. Razorpay subscription auto-renews monthly.
- **`unlimited`** users (admin-grandfathered, e.g. Lakshmi from the previous model): always bypass. Used for "lifetime customer" cases.

Schema in [supabase/migrations/0003_subscription.sql](../supabase/migrations/0003_subscription.sql).
Razorpay subscription setup steps in [razorpay-subscription-setup.md](razorpay-subscription-setup.md).

---

## Find a user by email

Every operation starts here:

```sql
select u.id as user_id,
       u.email,
       p.plan_tier,
       p.extra_granted,
       p.razorpay_subscription_id,
       p.subscription_status,
       p.current_period_end,
       p.paid_at
  from auth.users u
  left join public.profiles p on p.user_id = u.id
 where u.email = 'customer@example.com';
```

What to look for:
- `plan_tier = 'unlimited'` → fully grandfathered, never paywalled
- `plan_tier = 'pro' AND current_period_end > now()` → paid + active
- `plan_tier = 'pro' AND current_period_end <= now()` → paid but lapsed (treated as free)
- `plan_tier = 'free_hosted'` → daily 10 + extra_granted cap
- `subscription_status` mirrors Razorpay state: `active` / `cancelled` / `halted` / etc.

---

## Mark a customer as grandfathered-unlimited

Use for: yourself, your team, anyone who paid under an old model, or
a free-pass for a key prospect.

```sql
update public.profiles
   set plan_tier  = 'unlimited',
       updated_at = now()
 where user_id = (select id from auth.users where email = 'customer@example.com');
```

Their daily counter stops being checked entirely.

---

## Grant +N daily enhancements on the free tier

For free users who need a higher daily ceiling without going to pro.
Added on top of the 10/day default — running this twice adds twice as many.

```sql
update public.profiles
   set extra_granted = extra_granted + 5,
       updated_at    = now()
 where user_id = (select id from auth.users where email = 'customer@example.com');
```

`extra_granted = 5` → their daily cap becomes 15 per IST day instead of 10.

---

## Manually flip someone to pro (skip Razorpay)

For: comp accounts, friends-and-family pricing, bug-bounty rewards,
edge cases where Razorpay's webhook didn't land.

```sql
update public.profiles
   set plan_tier          = 'pro',
       subscription_status = 'active',
       current_period_end  = now() + interval '30 days',
       paid_at             = now(),
       updated_at          = now()
 where user_id = (select id from auth.users where email = 'customer@example.com');
```

After 30 days they revert to free unless you re-run this or a real
Razorpay subscription kicks in.

---

## Revoke a subscription

For chargebacks, refunds, accidental upgrades:

```sql
update public.profiles
   set plan_tier               = 'free_hosted',
       subscription_status     = 'cancelled',
       current_period_end      = null,
       razorpay_subscription_id = null,
       updated_at              = now()
 where user_id = (select id from auth.users where email = 'customer@example.com');
```

**Important**: this only changes our database. If they have a real
active Razorpay subscription, you must ALSO cancel it in the Razorpay
dashboard → Subscriptions → find their `sub_xxx` → Cancel — otherwise
their card keeps getting charged.

---

## Reset a free user's daily count

Testing or making good after a glitch:

```sql
delete from public.daily_usage
 where user_id = (select id from auth.users where email = 'customer@example.com')
   and usage_date = (now() at time zone 'Asia/Kolkata')::date;
```

---

## Audit: active subscribers

```sql
select u.email,
       p.current_period_end,
       p.subscription_status,
       p.paid_at
  from public.profiles p
  join auth.users u on u.id = p.user_id
 where p.plan_tier = 'pro'
   and p.current_period_end > now()
 order by p.current_period_end asc;
```

## Audit: subscriptions about to lapse this week

```sql
select u.email,
       p.current_period_end,
       p.subscription_status
  from public.profiles p
  join auth.users u on u.id = p.user_id
 where p.plan_tier = 'pro'
   and p.current_period_end between now() and now() + interval '7 days'
 order by p.current_period_end asc;
```

If `subscription_status = 'cancelled'` for any of these, the customer
chose to cancel; their access dies on `current_period_end`. If
`subscription_status = 'active'`, Razorpay will auto-charge them and
the webhook will extend `current_period_end` automatically.

## Audit: today's free users hitting the cap

```sql
select u.email, du.count, p.extra_granted, (10 + p.extra_granted) as effective_limit
  from public.daily_usage du
  join auth.users u   on u.id = du.user_id
  join public.profiles p on p.user_id = du.user_id
 where du.usage_date = (now() at time zone 'Asia/Kolkata')::date
   and du.count >= (10 + p.extra_granted - 2)
   and p.plan_tier = 'free_hosted'
 order by du.count desc;
```

Useful for outreach — these users are right at the cap and might be
the most convertible.

---

## Legacy columns

The `lifetime_used`, `paid_at`-as-lifetime-flag from the brief ₹199
model are kept in the schema but unused. The new `paid_at` semantic is
"when their current subscription cycle started"; it's overwritten on
every renewal.
