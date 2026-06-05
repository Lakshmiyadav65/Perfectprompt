-- PerfectPrompt — enhancements table + RLS
-- ============================================================
-- Run this in the Supabase SQL Editor (Database → SQL Editor →
-- New query → paste → Run). Safe to re-run: every statement uses
-- IF NOT EXISTS / OR REPLACE.
--
-- Purpose: server-side mirror of the local
-- <app_data_dir>/enhancement_history.jsonl file. Every successful
-- pipeline run on the client also POSTs the same record here so
-- the user's history survives device wipes, supports a future
-- cross-device dashboard, and gives us a single place to audit
-- what's been enhanced.
--
-- Data flow (Rust):
--   pipeline::run (stage F, success)
--     → enhancement_history::append (local JSONL write)
--     → tokio::spawn → enhancement_history::sync_to_remote
--          → POST {SUPABASE_URL}/rest/v1/enhancements
--             headers: apikey + Bearer <user_jwt>
--             body: { id, created_at, rough, enhanced, route,
--                     project_id, project_name }
--
-- Auth: every insert is RLS-gated to the user's own auth.uid().
-- The client ships the user's JWT in the Authorization header.

-- ────────────────────────────────────────────────────────────
-- 0. Pre-flight (only needed if a previous run failed midway)
-- ────────────────────────────────────────────────────────────
-- If you previously ran an older / partial version of this file
-- and ended up with an `enhancements` table that's missing
-- columns (e.g. "column \"rough\" does not exist" when the
-- unique index runs), uncomment the line below ONCE to drop the
-- broken shell so the CREATE TABLE below can build a fresh one.
-- DO NOT leave this uncommented on subsequent runs — it will
-- wipe any data already synced from the client.
--
-- drop table if exists public.enhancements cascade;

-- ────────────────────────────────────────────────────────────
-- 1. Table
-- ────────────────────────────────────────────────────────────

create table if not exists public.enhancements (
  -- Client-generated id (`make_id` in Rust: nanos + content fnv).
  -- We don't regenerate it server-side so the same record on
  -- client and server can be matched 1:1 for upsert / dedup.
  id text primary key,

  user_id uuid not null references auth.users (id) on delete cascade,

  -- Persisted ISO-8601 (UTC) from the client. Mirrors created_at
  -- in the local JSONL — used for chronological sort + display.
  created_at timestamptz not null default now (),

  rough text not null,
  enhanced text not null,
  route text not null,

  project_id text,
  project_name text,

  -- Server-side bookkeeping. inserted_at is *server time* so we
  -- can tell when sync actually landed even if the client clock
  -- is off.
  inserted_at timestamptz not null default now (),

  -- ── Validation ──
  constraint enhancements_rough_nonempty check (length(trim(rough)) > 0),
  constraint enhancements_enhanced_nonempty check (length(trim(enhanced)) > 0),
  constraint enhancements_rough_len check (length(rough) <= 20000),
  constraint enhancements_enhanced_len check (length(enhanced) <= 20000),
  constraint enhancements_route_enum check (route in ('code', 'writing', 'generic')),
  constraint enhancements_project_id_len check (
    project_id is null or length(project_id) <= 100
  ),
  constraint enhancements_project_name_len check (
    project_name is null or length(project_name) <= 200
  )
);

-- Fast lookup of "my recent enhancements". Covers the dashboard's
-- list endpoint (filter by user_id, sort by created_at desc).
create index if not exists enhancements_user_created_idx
  on public.enhancements (user_id, created_at desc);

-- Content-hash index (so server-side dedup matches the client's
-- "skip if same content recently" rule). Two rows with the same
-- (user_id, rough, enhanced) within a single session are almost
-- certainly the result of an accidental double-fire — block them
-- at the database level.
--
-- The expression columns are wrapped in explicit parens so the
-- CREATE INDEX parser unambiguously treats them as expressions
-- rather than column references — Postgres usually accepts the
-- bare function-call form, but the explicit version side-steps
-- any version-specific edge case.
create unique index if not exists enhancements_user_content_idx
  on public.enhancements (
    user_id,
    (md5(trim(rough))),
    (md5(trim(enhanced)))
  );

-- ────────────────────────────────────────────────────────────
-- 2. RLS
-- ────────────────────────────────────────────────────────────

alter table public.enhancements enable row level security;

drop policy if exists "users select own enhancements" on public.enhancements;
create policy "users select own enhancements"
  on public.enhancements
  for select
  using (auth.uid () = user_id);

drop policy if exists "users insert own enhancements" on public.enhancements;
create policy "users insert own enhancements"
  on public.enhancements
  for insert
  with check (auth.uid () = user_id);

drop policy if exists "users update own enhancements" on public.enhancements;
create policy "users update own enhancements"
  on public.enhancements
  for update
  using (auth.uid () = user_id)
  with check (auth.uid () = user_id);

drop policy if exists "users delete own enhancements" on public.enhancements;
create policy "users delete own enhancements"
  on public.enhancements
  for delete
  using (auth.uid () = user_id);

-- ────────────────────────────────────────────────────────────
-- 3. Trigger: backfill user_id from auth.uid() on insert
-- ────────────────────────────────────────────────────────────
-- Client doesn't ship user_id in the row body — the server derives
-- it from the JWT so a misbehaving client can't insert rows under
-- a different user_id. The RLS `with check` policy above already
-- enforces this, but the trigger lets the client send a minimal
-- payload (id, rough, enhanced, route, ...) without having to
-- duplicate the user id it already authenticated as.

create or replace function public.enhancements_set_user_id ()
returns trigger
language plpgsql
security definer
set search_path = public
as $$
begin
  if new.user_id is null then
    new.user_id := auth.uid ();
  elsif new.user_id <> auth.uid () then
    raise exception 'user_id mismatch: cannot insert rows for other users';
  end if;
  return new;
end;
$$;

drop trigger if exists enhancements_set_user_id_trg on public.enhancements;
create trigger enhancements_set_user_id_trg
  before insert on public.enhancements
  for each row
  execute function public.enhancements_set_user_id ();
