-- =============================================================================
-- PerfectPrompt — Paywall migration (v1)
-- =============================================================================
-- Created 2026-05-19.
--
-- Replaces the per-day quota model from 0001 with a lifetime free-trial
-- model + admin grants + a paid bypass:
--   - free_hosted users: 10 lifetime enhancements + extra_granted (admin)
--   - pro / unlimited users: bypass the lifetime cap entirely (paid)
--   - Manual paid flow: admin flips plan_tier='pro' after Razorpay confirms
--
-- Migration safety: additive only. The old daily_usage table and
-- consume_quota function stay in place (callable but unused) so a partial
-- rollout doesn't break anything. A future cleanup migration can drop
-- them once we're confident nothing depends on them.
--
-- Existing-user behaviour: lifetime_used defaults to 0 so users who'd
-- already burned 20+ on the daily model get a clean 10 to spend rather
-- than landing locked-out on the morning of the swap.
-- =============================================================================

-- ----------------------------------------------------------------------------
-- 1. profiles: new columns
-- ----------------------------------------------------------------------------
alter table public.profiles
  add column if not exists lifetime_used int         not null default 0 check (lifetime_used >= 0),
  add column if not exists extra_granted int         not null default 0 check (extra_granted >= 0),
  add column if not exists paid_at       timestamptz;

comment on column public.profiles.lifetime_used is
  'Successful enhancements consumed under the lifetime free-trial model. Incremented by consume_lifetime_quota.';
comment on column public.profiles.extra_granted is
  'Admin-granted bonus enhancements on top of the free trial cap. UPDATE manually to add +N for a specific user.';
comment on column public.profiles.paid_at is
  'Timestamp of the most recent paid upgrade. Set by admin after Razorpay receipt.';

-- ----------------------------------------------------------------------------
-- 2. Free-trial cap (centralised; consume_lifetime_quota reads from here)
-- ----------------------------------------------------------------------------
create or replace function public.lifetime_free_limit()
returns int
language sql
immutable
as $$
  select 10;
$$;

-- ----------------------------------------------------------------------------
-- 3. consume_lifetime_quota: atomic "try to consume one lifetime unit"
-- ----------------------------------------------------------------------------
-- Mirrors consume_quota's contract so the edge function only changes which
-- RPC name it calls. Differences:
--   - "limit" is now lifetime: 10 + extra_granted (or unlimited for paid)
--   - pro / unlimited plans bypass the cap and DON'T tick the counter
--     so an admin can mark a payer as 'pro' and they're unblocked forever
--     without burning their extra_granted balance
--   - For free_hosted, the increment uses an UPDATE ... WHERE lifetime_used
--     < limit pattern so the predicate is part of the row lock — concurrent
--     hotkey presses can't both squeeze past the cap
--
-- Returns one row:
--   allowed     — true if the unit was consumed (or bypassed for paid)
--   used        — lifetime_used after the attempt (informational for paid)
--   quota_limit — effective ceiling: 10 + extra_granted for free; INT_MAX for paid
--   plan_tier   — 'free_hosted' | 'pro' | 'unlimited'

create or replace function public.consume_lifetime_quota(p_user_id uuid)
returns table (
  allowed     boolean,
  used        int,
  quota_limit int,
  plan_tier   text
)
language plpgsql
security definer
set search_path = public, pg_temp
as $$
declare
  v_plan_tier  text;
  v_extra      int;
  v_limit      int;
  v_used_after int;
begin
  -- Look up the profile (create on the fly if the trigger missed it).
  select p.plan_tier, p.extra_granted, p.lifetime_used
    into v_plan_tier, v_extra, v_used_after
    from public.profiles p
   where p.user_id = p_user_id;

  if v_plan_tier is null then
    insert into public.profiles (user_id) values (p_user_id)
      on conflict (user_id) do nothing;
    v_plan_tier  := 'free_hosted';
    v_extra      := 0;
    v_used_after := 0;
  end if;

  -- Paid / unlimited bypass: don't tick the counter, just allow.
  if v_plan_tier in ('pro', 'unlimited') then
    return query select true, v_used_after, 2147483647, v_plan_tier;
    return;
  end if;

  -- Free tier: 10 + extra_granted lifetime cap.
  v_limit := public.lifetime_free_limit() + coalesce(v_extra, 0);

  -- Atomic increment-if-under-cap. The WHERE clause inside UPDATE makes the
  -- predicate part of the row lock, so two concurrent calls cannot both
  -- increment past the cap.
  update public.profiles
     set lifetime_used = lifetime_used + 1,
         updated_at    = now()
   where user_id = p_user_id
     and lifetime_used < v_limit
  returning lifetime_used into v_used_after;

  if v_used_after is null then
    -- Predicate filtered us out → over cap. Read current count so the
    -- caller can render an accurate "X/Y used" message.
    select p.lifetime_used into v_used_after
      from public.profiles p
     where p.user_id = p_user_id;
    return query select false, v_used_after, v_limit, v_plan_tier;
  else
    return query select true, v_used_after, v_limit, v_plan_tier;
  end if;
end;
$$;

grant execute on function public.consume_lifetime_quota(uuid) to authenticated;
grant execute on function public.lifetime_free_limit() to authenticated;
