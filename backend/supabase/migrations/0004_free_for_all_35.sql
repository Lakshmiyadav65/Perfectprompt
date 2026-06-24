-- =============================================================================
-- PerfectPrompt — "Free for all" launch promo (temporary)
-- =============================================================================
-- Created 2026-06-05.
--
-- Context: at launch the ₹99/mo paywall was suppressing adoption — we need
-- volume before we meter. So for the first few days every signed-in user
-- gets a generous FREE daily allowance and nobody is pushed to pay. We are
-- NOT removing the payment system: Razorpay, the subscription edge
-- functions, and the pro / unlimited tiers all stay wired up so we can
-- flip metering back on without a code change.
--
-- The single lever here is the free daily cap. consume_daily_quota (from
-- 0003) already reads its ceiling from daily_free_limit(); we just bump
-- that function from 10 → 35. Every free_hosted user now gets 35
-- enhancements per IST day (+ extra_granted, unchanged). pro / unlimited
-- bypass the cap exactly as before.
--
-- Why 35: enough that almost nobody hits it in normal daily use, while
-- still capping a single account's burn on our hosted Groq key until we
-- have real usage data to size a permanent free tier.
--
-- Migration safety: additive, idempotent (CREATE OR REPLACE). No data
-- touched, no columns changed. consume_daily_quota picks up the new
-- ceiling on its next call — no other deploy needed.
--
-- REVERTING TO PAID: when we re-enable metering, ship a follow-up
-- migration that sets this back to `select 10;` (or whatever the
-- permanent free tier becomes). Nothing else has to change — the client
-- reads the limit from the server, and the upgrade UI is still in place.
-- Keep the client constant DAILY_FREE_LIMIT (src/hooks/useEnhancementUsage.ts)
-- in sync with this value so the mount-time sidebar paint and the
-- "Special access" (admin-grant) detection stay correct.
-- =============================================================================

create or replace function public.daily_free_limit()
returns int
language sql
immutable
as $$
  select 35;
$$;

comment on function public.daily_free_limit() is
  'Free-tier daily enhancement cap (IST-bucketed), read by consume_daily_quota. '
  'Temporarily 35 for the launch "free for all" promo; was 10 under the paid '
  'model. Lower it back in a follow-up migration to re-meter.';

grant execute on function public.daily_free_limit() to authenticated;
