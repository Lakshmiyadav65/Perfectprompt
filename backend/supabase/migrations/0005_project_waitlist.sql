-- =============================================================================
-- PerfectPrompt — Project Knowledge "beta waitlist" table
-- =============================================================================
-- Created 2026-06-29.
--
-- The Projects (Project Knowledge / Project Context) feature ships behind a
-- BETA badge and a prominent in-app notice telling users it's still being
-- finished. That notice invites them to join a waitlist so we can email them
-- when it launches. This table is where those one-click signups land.
--
-- Because signup is mandatory (every user is authenticated), "join the
-- waitlist" needs no email entry form — the client upserts the signed-in
-- user's id + email here in a single click. One row per user (user_id is the
-- PK), so clicking twice is a harmless no-op.
--
-- Migration safety: additive only. New table + RLS, nothing else touched.
-- Apply with `supabase db push` from backend/.
--
-- Querying signups (admin): the rows are readable with the service-role key
--   select email, created_at from public.project_waitlist order by created_at;
-- =============================================================================

create table if not exists public.project_waitlist (
  user_id    uuid primary key references auth.users (id) on delete cascade,
  email      text,
  created_at timestamptz not null default now()
);

comment on table public.project_waitlist is
  'One row per user who opted into the Projects (Project Knowledge) beta '
  'waitlist from the in-app banner. user_id is the auth.users id; email is '
  'denormalised so admins can export the list without joining auth.users.';

-- Recent-signups-first ordering for the admin export query.
create index if not exists project_waitlist_created_at_idx
  on public.project_waitlist (created_at desc);

-- ----------------------------------------------------------------------------
-- Row-level security
-- ----------------------------------------------------------------------------
-- A user may add themselves and check their own status — nothing more. No
-- update/delete policies: a waitlist entry isn't something a user edits, and
-- omitting them means the only way to leave the list is admin SQL (fine).
alter table public.project_waitlist enable row level security;

-- See only your own row, so the client can render the "you're on the list"
-- confirmed state on return visits.
create policy "project_waitlist_select_own"
  on public.project_waitlist
  for select
  to authenticated
  using (auth.uid() = user_id);

-- Add yourself exactly once. The PK on user_id turns a second insert into a
-- conflict, which the client sends as ignore-duplicates (a clean no-op).
create policy "project_waitlist_insert_self"
  on public.project_waitlist
  for insert
  to authenticated
  with check (auth.uid() = user_id);

grant select, insert on public.project_waitlist to authenticated;
