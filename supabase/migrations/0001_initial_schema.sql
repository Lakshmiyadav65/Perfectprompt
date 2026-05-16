-- =============================================================================
-- PerfectPrompt backend schema (v0)
-- =============================================================================
-- Created 2026-05-16.
--
-- Tables:
--   profiles       — 1:1 with auth.users; plan tier, override, Stripe link
--   enhancements   — per-call log (route, latency, success; never prompt text)
--   daily_usage    — denormalized counter, primary quota check
--   subscriptions  — Stripe state, schema-ready but not yet wired
--
-- Quota enforcement is atomic via public.consume_quota(p_user_id).
--
-- Writes flow exclusively through edge functions using the service_role key,
-- which bypasses RLS. RLS policies grant authenticated users SELECT on their
-- own rows only — no INSERT/UPDATE/DELETE policies because the client never
-- writes directly.
-- =============================================================================

create extension if not exists "pgcrypto";

-- -----------------------------------------------------------------------------
-- profiles
-- -----------------------------------------------------------------------------
create table public.profiles (
  user_id              uuid primary key references auth.users(id) on delete cascade,
  plan_tier            text not null default 'free_hosted'
                         check (plan_tier in ('free_hosted', 'pro', 'unlimited')),
  daily_limit_override int check (daily_limit_override is null or daily_limit_override >= 0),
  stripe_customer_id   text unique,
  created_at           timestamptz not null default now(),
  updated_at           timestamptz not null default now()
);

comment on column public.profiles.daily_limit_override is
  'Overrides plan_daily_limit() for this user. NULL means use the plan default.';

-- Plan-tier defaults. Edit here; consume_quota() and the dashboard both read this.
create or replace function public.plan_daily_limit(p_plan_tier text)
returns int
language sql
immutable
as $$
  select case p_plan_tier
    when 'free_hosted' then 50
    when 'pro'         then 1000
    when 'unlimited'   then 1000000
    else 0
  end;
$$;

-- -----------------------------------------------------------------------------
-- enhancements (per-call log)
-- -----------------------------------------------------------------------------
create table public.enhancements (
  id          uuid primary key default gen_random_uuid(),
  user_id     uuid not null references auth.users(id) on delete cascade,
  created_at  timestamptz not null default now(),
  route       text not null check (route in ('code', 'writing', 'generic')),
  latency_ms  int check (latency_ms is null or latency_ms >= 0),
  success     boolean not null,
  error_kind  text
);

create index enhancements_user_id_created_at_idx
  on public.enhancements (user_id, created_at desc);

-- -----------------------------------------------------------------------------
-- daily_usage (denormalized counter; one row per user per UTC date)
-- -----------------------------------------------------------------------------
create table public.daily_usage (
  user_id    uuid not null references auth.users(id) on delete cascade,
  usage_date date not null,
  count      int  not null default 0 check (count >= 0),
  primary key (user_id, usage_date)
);

-- -----------------------------------------------------------------------------
-- subscriptions (Stripe; schema-ready, not yet wired)
-- -----------------------------------------------------------------------------
create table public.subscriptions (
  user_id                uuid primary key references auth.users(id) on delete cascade,
  stripe_subscription_id text unique,
  status                 text not null,  -- 'active' | 'past_due' | 'canceled' | 'trialing' | ...
  current_period_end     timestamptz,
  updated_at             timestamptz not null default now()
);

-- -----------------------------------------------------------------------------
-- Trigger: auto-create profile on auth.users insert
-- -----------------------------------------------------------------------------
create or replace function public.handle_new_user()
returns trigger
language plpgsql
security definer
set search_path = public, pg_temp
as $$
begin
  insert into public.profiles (user_id) values (new.id)
    on conflict (user_id) do nothing;
  return new;
end;
$$;

create trigger on_auth_user_created
  after insert on auth.users
  for each row execute function public.handle_new_user();

-- -----------------------------------------------------------------------------
-- consume_quota: atomic "try to consume one quota unit for today"
-- -----------------------------------------------------------------------------
-- Returns one row:
--   allowed     — true if the unit was consumed, false if over cap
--   used        — count after the attempted increment (the user's view of "X/Y today")
--   daily_limit — effective cap (override or plan default)
--   plan_tier   — the user's current plan
--
-- Race-safe: the increment is a single INSERT ... ON CONFLICT DO UPDATE
-- whose UPDATE branch carries the "count < limit" predicate. Concurrent
-- hotkey presses contend on the (user_id, usage_date) row, so two
-- simultaneous requests cannot both succeed once the cap is hit.
create or replace function public.consume_quota(p_user_id uuid)
returns table (
  allowed     boolean,
  used        int,
  daily_limit int,
  plan_tier   text
)
language plpgsql
security definer
set search_path = public, pg_temp
as $$
declare
  v_plan_tier  text;
  v_override   int;
  v_limit      int;
  v_today      date := (now() at time zone 'utc')::date;
  v_new_count  int;
begin
  -- 1. Look up plan and effective limit.
  select p.plan_tier, p.daily_limit_override
    into v_plan_tier, v_override
    from public.profiles p
   where p.user_id = p_user_id;

  if v_plan_tier is null then
    -- Profile should exist via the on_auth_user_created trigger; guard anyway.
    insert into public.profiles (user_id) values (p_user_id)
      on conflict (user_id) do nothing;
    v_plan_tier := 'free_hosted';
    v_override  := null;
  end if;

  v_limit := coalesce(v_override, public.plan_daily_limit(v_plan_tier));

  if v_limit <= 0 then
    return query select false, 0, v_limit, v_plan_tier;
    return;
  end if;

  -- 2. Atomic increment-if-under-limit.
  insert into public.daily_usage as du (user_id, usage_date, count)
  values (p_user_id, v_today, 1)
  on conflict (user_id, usage_date) do update
    set count = du.count + 1
    where du.count < v_limit
  returning du.count into v_new_count;

  if v_new_count is null then
    -- Conflict fired but the UPDATE predicate filtered us out → over quota.
    select du.count into v_new_count
      from public.daily_usage du
     where du.user_id = p_user_id and du.usage_date = v_today;

    return query select false, v_new_count, v_limit, v_plan_tier;
  else
    return query select true, v_new_count, v_limit, v_plan_tier;
  end if;
end;
$$;

-- -----------------------------------------------------------------------------
-- RLS
-- -----------------------------------------------------------------------------
alter table public.profiles      enable row level security;
alter table public.enhancements  enable row level security;
alter table public.daily_usage   enable row level security;
alter table public.subscriptions enable row level security;

-- SELECT-own policies. No INSERT/UPDATE/DELETE policies: writes go through
-- edge functions using the service_role key, which bypasses RLS entirely.
create policy "profiles_select_own"      on public.profiles
  for select using (auth.uid() = user_id);

create policy "enhancements_select_own"  on public.enhancements
  for select using (auth.uid() = user_id);

create policy "daily_usage_select_own"   on public.daily_usage
  for select using (auth.uid() = user_id);

create policy "subscriptions_select_own" on public.subscriptions
  for select using (auth.uid() = user_id);

-- Allow authenticated users to call consume_quota for themselves.
-- (Edge functions using service_role bypass this; the grant just keeps
-- the option open for client-side "how much do I have left?" calls.)
grant execute on function public.consume_quota(uuid) to authenticated;
grant execute on function public.plan_daily_limit(text) to authenticated;
