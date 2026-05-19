-- =============================================================================
-- PerfectPrompt — Subscription model migration (v1)
-- =============================================================================
-- Created 2026-05-19.
--
-- Replaces the lifetime ₹199 model from 0002 with a daily-free + monthly-pro
-- subscription model:
--   - free_hosted users: 10 enhancements per IST day (+ extra_granted bonus)
--   - pro users: unlimited, but only while current_period_end > now()
--   - unlimited users (admin grant, grandfather Lakshmi): always bypass
--
-- Razorpay Subscriptions API is the underlying payment primitive.
-- The webhook receives subscription.activated/charged/cancelled/halted
-- events and updates the columns added here.
--
-- Migration safety: additive only. The lifetime_used/extra_granted/paid_at
-- columns from 0002 stay — extra_granted now means "+N per IST day" instead
-- of "+N lifetime", but the column is reused without rename so the admin
-- SQL keeps working. lifetime_used becomes informational (no longer read).
--
-- Lazy expiry: when a pro user's current_period_end passes, they're not
-- auto-downgraded by a cron. Instead, the new RPC checks the date on
-- every enhancement attempt and treats stale pros as free_hosted. The
-- webhook downgrades them on the next subscription.cancelled event
-- (which Razorpay emits eventually), or admin SQL can be used.
-- =============================================================================

-- ----------------------------------------------------------------------------
-- 1. profiles: subscription tracking columns
-- ----------------------------------------------------------------------------
alter table public.profiles
  add column if not exists razorpay_customer_id      text,
  add column if not exists razorpay_subscription_id  text unique,
  add column if not exists subscription_status       text
                            check (subscription_status is null
                                   or subscription_status in (
                                     'created', 'authenticated', 'active',
                                     'pending', 'halted', 'cancelled',
                                     'completed', 'expired', 'paused'
                                   )),
  add column if not exists current_period_end        timestamptz,
  add column if not exists subscription_short_url    text;

comment on column public.profiles.razorpay_customer_id is
  'Razorpay customer ID (cust_xxx). Created on first subscribe; reused on subsequent subscriptions.';
comment on column public.profiles.razorpay_subscription_id is
  'Razorpay subscription ID (sub_xxx). Updated by create-subscription edge function on signup.';
comment on column public.profiles.subscription_status is
  'Mirrors Razorpay subscription.status field. Active means card is being charged successfully.';
comment on column public.profiles.current_period_end is
  'When the current paid month ends. Updated by webhook on subscription.charged. Pro access is granted while this is in the future.';
comment on column public.profiles.subscription_short_url is
  'Razorpay-hosted subscription page URL. Customer can revisit it to manage / cancel. Returned by /v1/subscriptions on creation.';

-- ----------------------------------------------------------------------------
-- 2. Free daily limit (centralised; consume_daily_quota reads from here)
-- ----------------------------------------------------------------------------
create or replace function public.daily_free_limit()
returns int
language sql
immutable
as $$
  select 10;
$$;

-- ----------------------------------------------------------------------------
-- 3. consume_daily_quota: atomic "try to consume one daily-or-subscription unit"
-- ----------------------------------------------------------------------------
-- Mirrors the contract of the older consume_quota / consume_lifetime_quota
-- so the edge function only changes which RPC name it calls. Differences:
--   - Decides between three paths based on plan_tier + current_period_end:
--     1. unlimited tier (admin grant / Lakshmi) → always allow, no count
--     2. pro tier + period not expired → always allow, no count
--     3. pro tier + period expired OR free tier → daily count check
--   - Daily resets at midnight Asia/Kolkata (not UTC), because most users
--     are in India and "my day rolls over at 5:30 AM" is confusing
--   - Atomic increment uses INSERT … ON CONFLICT DO UPDATE with a
--     count-vs-limit predicate inside, same race-safe pattern as the
--     daily consume_quota from 0001
--
-- Returns one row:
--   allowed     — true if the unit was consumed (or bypassed for paid)
--   used        — daily usage_count after the attempt
--   quota_limit — daily ceiling: 10 + extra_granted for free; INT_MAX for paid
--   plan_tier   — effective tier ('free_hosted' | 'pro' | 'unlimited');
--                 stale-pro users get reported as 'free_hosted' here so
--                 the client knows their subscription lapsed
--   subscription_active — bool: did the pro path actually apply?

create or replace function public.consume_daily_quota(p_user_id uuid)
returns table (
  allowed             boolean,
  used                int,
  quota_limit         int,
  plan_tier           text,
  subscription_active boolean
)
language plpgsql
security definer
set search_path = public, pg_temp
as $$
declare
  v_plan_tier      text;
  v_extra          int;
  v_period_end     timestamptz;
  v_now            timestamptz := now();
  v_today          date        := (v_now at time zone 'Asia/Kolkata')::date;
  v_limit          int;
  v_new_count      int;
  v_pro_valid      boolean;
begin
  -- Load profile (lazy-create if the trigger missed it)
  select p.plan_tier, p.extra_granted, p.current_period_end
    into v_plan_tier, v_extra, v_period_end
    from public.profiles p
   where p.user_id = p_user_id;

  if v_plan_tier is null then
    insert into public.profiles (user_id) values (p_user_id)
      on conflict (user_id) do nothing;
    v_plan_tier  := 'free_hosted';
    v_extra      := 0;
    v_period_end := null;
  end if;

  -- Path 1: admin-grandfathered unlimited tier — always allow, never count
  if v_plan_tier = 'unlimited' then
    return query select true, 0, 2147483647, 'unlimited'::text, true;
    return;
  end if;

  -- Path 2: pro tier with a live period — allow, never count
  v_pro_valid := (v_plan_tier = 'pro' and v_period_end is not null and v_period_end > v_now);
  if v_pro_valid then
    return query select true, 0, 2147483647, 'pro'::text, true;
    return;
  end if;

  -- Path 3: free_hosted, OR pro whose subscription lapsed (effectively free now).
  -- Daily cap: 10 + extra_granted, IST-bucketed.
  v_limit := public.daily_free_limit() + coalesce(v_extra, 0);

  insert into public.daily_usage as du (user_id, usage_date, count)
    values (p_user_id, v_today, 1)
    on conflict (user_id, usage_date) do update
      set count = du.count + 1
     where du.count < v_limit
  returning du.count into v_new_count;

  if v_new_count is null then
    -- Conflict resolved but predicate filtered us out → over cap
    select du.count into v_new_count
      from public.daily_usage du
     where du.user_id = p_user_id and du.usage_date = v_today;
    return query select false, coalesce(v_new_count, v_limit),
                        v_limit, 'free_hosted'::text, false;
  else
    return query select true, v_new_count, v_limit, 'free_hosted'::text, false;
  end if;
end;
$$;

grant execute on function public.consume_daily_quota(uuid) to authenticated;
grant execute on function public.daily_free_limit() to authenticated;

-- ----------------------------------------------------------------------------
-- 4. Helpful indexes
-- ----------------------------------------------------------------------------
create index if not exists profiles_subscription_id_idx
  on public.profiles (razorpay_subscription_id)
  where razorpay_subscription_id is not null;

create index if not exists profiles_period_end_idx
  on public.profiles (current_period_end)
  where plan_tier = 'pro';
