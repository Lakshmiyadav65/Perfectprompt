# PromptForge — Phase 2 Handoff

Picking up where the previous session left off. This document is the
single source of truth for what's been done, what's in-flight, and
what's blocked — written for whoever comes in fresh (human or agent).

---

## TL;DR

1. **Phase 1 migration is committed and pushed.** Branch
   `phase-1-migration` at SHA `cb490cb` on `origin
   = github.com/Lakshmiyadav65/prompt.git`. That branch contains the
   intake → cache → router → LLM → validate → trace pipeline + the
   three route prompts (`code-enhancer.md`, `writing-enhancer.md`,
   `generic-enhancer.md`) + the questions prompt.
2. **All Phase 2 work is uncommitted** on top of that snapshot,
   currently in the working tree of `C:\Users\haris\Projects\prompt-main`.
   `git diff --stat HEAD` = **21 files changed, +2511 / −313** (see
   §10 for the full list).
3. **Phase 2 is structurally complete.** 10-step build order shipped,
   plus the post-Pass-1 fixes (`reject_if_outsources_content`
   validator, extended Mode B NEVER, tightened convention block,
   Generic temperature 0.4→0.3), plus a Beta-badge UI pass on the
   Projects feature, plus the GroqError + rate-limit-notification
   feature. **200/200 tests pass; zero warnings.**
4. **Eval Pass 2 (re-run after the fixes) never produced clean
   numbers.** Groq's 100,000-token daily quota was exhausted before
   the harness could land a clean run. See §6.
5. **Open user decision: notification surface for the rate-limit
   path.** The Rust pipeline correctly classifies rate-limit hits,
   logs them, writes them to trace as `validation_outcome:
   "fallback_rate_limit"`, and updates the tray-icon tooltip for 1.5s —
   but no OS-level toast appears, because `tauri-plugin-notification`
   isn't installed. Three options at the bottom of §11.

---

## 1. Project context (one paragraph)

**PromptForge** is a Tauri 2 + Rust + React 19 system-tray app that
turns rough prompt text into precise prompts via a global hotkey
(`Ctrl+Alt+E`). It captures the user's selection via Win32 SendInput,
runs an enhancement pipeline that hits Groq's
`llama-3.3-70b-versatile`, and pastes the rewrite over the original
selection. Phase 1 migrated from a single-prompt architecture to a
6-stage pipeline (intake → cache → router → LLM → validate →
deliver). Phase 2 added project context — when the user has an
active project, a `<context>...</context>` block is prepended to the
LLM user message so the rewrite is grounded in the project's stack,
tooling, conventions, and description.

## 2. Repo and branches

| Path | Role | State |
|---|---|---|
| `C:\Users\haris\Projects\prompt-main` | **The live working tree.** Phase 2 work lives here. | Branch `phase-1-migration` at `cb490cb` + 21 uncommitted files |
| `C:\Users\haris\Downloads\prompt-main\prompt-main` | A stale ZIP extract of the upstream main branch (pre-migration). | Do not edit — used only as a reference for what the public main looked like before Phase 1 |
| `C:\Users\haris\Downloads\PromptEnhancer-main` | An even older ZIP from when the repo was called `PromptEnhancer` | Reference only |

Origin: `github.com/Lakshmiyadav65/prompt.git`. The Phase 1 migration
is on the remote on the `phase-1-migration` branch (pushed via the
push step in §3) — survives drive-loss.

Phase 2 work is uncommitted. **Recommended first action for any
follow-up agent: commit Phase 2 as `phase-2-context` branch on top of
`phase-1-migration`, then push.**

## 3. Phase 1 — what's already shipped (DO NOT REDO)

Verified via `docs/migration-report.md`. Phase 1's deliverables, all
on the `phase-1-migration` branch and in current working tree:

- **`pipeline.rs`** — `pipeline::run(app, PipelineInput) -> Result<PipelineOutput>` runs 6 stages:
  - A: intake (length gate, adversarial regex, SHA-256 fingerprint)
  - B: cache (256-entry LRU keyed on fingerprint)
  - C: router (domain + complexity + ambiguity → 5 routes: Decline, Bypass, Code, Writing, Generic)
  - D: LLM (single Groq call, route-specific knobs)
  - E: validate (strip preambles/fences, reject too-short/too-long/identical/likely-executed)
  - F: deliver (cache, JSONL trace, return)
- **Three route prompts** in `prompts/`: `code-enhancer.md`, `writing-enhancer.md`, `generic-enhancer.md`. Plus `questions-system-prompt.md` for the parallel question-card path.
- **Trace logging** to `<app_data_dir>/traces/YYYY-MM-DD.jsonl`. One record per pipeline invocation.
- **All paths fall back to the user's original input on failure.** Never throws an error at the user; the worst case is "your selection got pasted back unchanged."

Phase 1 had a known gap (§7.1 of migration-report): `notify_fallback`
is a tray-tooltip surrogate, not a real OS toast. This bites us in §8
of this handoff — fix is in §11.

## 4. Phase 2 — the 10-step build order

Original brief lived in chat (the user's "Project context —
implementation brief"). Steps as executed:

### Step 1 — `project_scan` returns structured `ProjectSummary`

- Replaced `scan_project_dir(path) -> Option<String>` with
  `scan_project_summary(path) -> Option<ProjectSummary>` in
  [project_scan.rs](../src-tauri/src/project_scan.rs).
- `ProjectSummary { stack, tooling, conventions, file_layout,
  readme_excerpt }` — all fields built by deterministic detection
  (Cargo.toml, package.json, lockfile, file-layout heuristics, README
  excerpt). No LLM call.
- 25 new tests; uses `std::sync::OnceLock` for regex caching (Phase 1
  §6.1 convention).

### Step 2 — `github_analyze` on-disk cache

- Added `pub struct CachedRepo { repo, fetched_at }` and pure
  `cache_file_path` / `cached_repo` / `fetch_and_cache` / `write_cache`
  / `iso_now` to [github_analyze.rs](../src-tauri/src/github_analyze.rs).
- Cache lives at `<app_config_dir>/project_cache/{project_id}.json`.
- 6 new tests cover round-trip, malformed JSON, missing cache,
  overwrite-on-refresh, dir-creation, path computation.

### Step 3 — `projects.rs` cache wiring

- Added `pub fn cached_context_path(app) -> Result<PathBuf>` (returns
  `<app_config_dir>/project_cache/`).
- Added private `pick_github_link(links)` + `maybe_spawn_github_fetch(app, project_id, links)`.
  Background fetch fires from `add_project` and `update_project` when
  any link parses as a github URL and no cache file exists yet.
- Added `pub async fn refresh_project_context(app, id) -> Result<String, String>`
  Tauri command for the manual-refresh button. Awaits the fetch and
  returns the new `fetched_at` timestamp.
- Added `pub fn get_cached_context_timestamp(app, id) -> Result<Option<String>, String>`
  Tauri command so `ProjectManager.tsx` can render "Last fetched:
  …" on form open without a refresh round-trip.
- Both new commands registered in `lib.rs` invoke handler.
- 4 new tests for `pick_github_link`.

### Step 4 — `build_context_block` in `pipeline.rs`

- Lives flat in [pipeline.rs](../src-tauri/src/pipeline.rs), not in a
  `pipeline/` directory (user reconciliation 4a).
- `pub(crate) fn build_context_block<R: Runtime>(app, project) ->
  Option<String>` — orchestrator-facing. Resolves the scan via
  `project_scan::scan_project_summary` and the cache via
  `github_analyze::cached_repo`, then delegates to the pure
  assembler.
- `pub fn assemble_context_block(project, scan, cached) ->
  Option<String>` — pure, testable. Source priority per the brief:
  description-derived Stack (when 2+ stack keywords present) →
  scan-derived Stack; Tooling/Conventions/file_layout from scan
  only; readme from scan first, then cache fallback.
- 2000-char ceiling enforced by `fit_context_to_budget`. Truncation
  priority: readme first, then file-layout, hard-cap core as last
  resort with a `... [truncated]` marker.
- 13 new tests covering the source priority, the budget enforcement,
  the `<context>` wrapping, the keyword detector (`stack_from_description`).

### Step 5 — Retire `developer_enhance.rs`

- File deleted.
- `mod developer_enhance;` removed from [lib.rs](../src-tauri/src/lib.rs).
- `developer_enhance` import dropped from [hotkey.rs](../src-tauri/src/hotkey.rs).
- `run_developer_path` deleted; the developer-classified branch now
  routes through `run_silent_path` (project context flows in
  uniformly via `pipeline::run`'s `build_context_block`, so a
  separate developer orchestration is no longer needed).
- `PipelineInput.context: Option<DeveloperContext>` field removed
  from [pipeline.rs](../src-tauri/src/pipeline.rs).
- `DeveloperContext` struct deleted.
- `build_user_message` signature changed: `(&str, Option<&str>)`
  where the second arg is the context block.
- Final verification: zero non-doc references to `developer_enhance`
  or `DeveloperContext` (verified via grep across `src-tauri/src/`).

### Step 6 — Wire context into `pipeline::run` + trace fields

- Stage D now resolves active project (post-cache-check, so cache
  hits don't pay the IO), calls `build_context_block`, sets
  `tr.context_present = bool` and `tr.effective_threshold = u32`.
- User message reordered: `<context>...</context>\n\n<input>...</input>`
  (context first, blank line, input second — per brief).
- [trace.rs](../src-tauri/src/trace.rs) `TraceRecord` gets two new
  fields with `#[serde(default)]` so legacy Phase 1 JSONL records
  still deserialize. Backward-compat test added.
- 4 new pipeline tests covering the user-message ordering and the
  trace defaults.

### Step 7 — Prompt updates (convention block + Mode B NEVER)

Convention block + Mode B NEVER added to all three route prompts:
[code-enhancer.md](../prompts/code-enhancer.md),
[writing-enhancer.md](../prompts/writing-enhancer.md),
[generic-enhancer.md](../prompts/generic-enhancer.md). Same text in
each.

**Important: the wording was tightened in the post-Pass-1 fix** (§7
below). Current text says "use that stack's idioms as the PRIMARY
pattern... not as one option among many" and gives concrete
examples ("Rust project → write `Result<T, E>`, `?`, `match`").

The Mode B NEVER was also extended in post-Pass-1 to cover named
subsystems/feature names, not just files/modules/functions.

### Step 8 — Router Mode D + calibration dry-run

- Extracted `pub(crate) const DECLINE_THRESHOLD: u32 = 70` and
  `pub(crate) const CONTEXT_THRESHOLD_BUMP: u32 = 0` in
  [router.rs](../src-tauri/src/router.rs).
- `pub fn run(input, context_present: bool) -> RouterOutput` —
  threads the flag through. `RouterOutput.effective_threshold` is
  populated (= `DECLINE_THRESHOLD + bump`).
- The Decline rule preserves the original compound gate:
  `if ambiguity >= effective_threshold && word_count < 5`. Mode D
  only relaxes the ambiguity side; the word_count gate stays.
- 5 new router tests.
- **Calibration dry-run found the brief's +15 hypothesis didn't
  hold** on this codebase's `score_ambiguity` heuristic. The five
  calibration inputs scored:

  | input | ambig | wc | no-ctx route | with-ctx route (at +15) |
  |---|---:|---:|---|---|
  | refactor the auth flow | 40 | 4 | Code | Code |
  | make it faster | 95 | 3 | Decline | Decline |
  | add error handling | 80 | 3 | Decline | **Generic** (over-fired) |
  | fix it | 95 | 2 | Decline | Decline |
  | update the dashboard layout | 50 | 4 | Generic | Generic |

  None of the inputs landed in the 70-84 band where +15 would have
  changed a routing decision toward the user's intent. User decision:
  **`CONTEXT_THRESHOLD_BUMP = 0`** (`Option D` in the dry-run
  report), with a `TODO(Phase 2.5)` comment pointing at re-tuning
  `score_ambiguity` later. Plumbing stays (consts, trace fields,
  router signature) so the wiring is verifiable end-to-end.

### Step 9 — `ProjectManager.tsx` Refresh button

- "Last fetched: …" row + "Refresh project context" button added
  beneath the Repo URL Analyze section in
  [ProjectManager.tsx](../src/components/ProjectManager.tsx).
- Visible only in edit mode (refresh requires a saved project).
  Button disabled when no github.com link is present in `formLinks`,
  with a tooltip explaining why.
- Frontend state: `lastFetched: string | null`, `refreshing: bool`.
  `openEditForm` fetches the cached timestamp via the new
  `get_cached_context_timestamp` Tauri command.
- CSS: `.pm-refresh-row`, `.pm-refresh-label`, `.pm-refresh-timestamp`
  in [ProjectManager.css](../src/components/ProjectManager.css).
- `formatLastFetched` converts the backend's `"{seconds}s"` format
  to a locale-rendered date string.
- TypeScript `npx tsc --noEmit` is clean.

### Step 10 — `eval_phase2_context.rs` harness + 5 acceptance tests

- New harness at [eval_phase2_context.rs](../src-tauri/examples/eval_phase2_context.rs).
- Sibling to `eval_pass2.rs` (Phase 1's harness). Run via
  `cargo run --example eval_phase2_context`.
- Test 1 (context bundle correctness): **PASS** — verbatim bundle
  for the Foo fixture project:
  ```
  <context>
  Project: Foo
  Stack: Tauri, React

  Description:
  Tauri 2 + React 19 app
  </context>
  ```
- Test 4 (rescoped to plumbing per user reconciliation): **PASS** —
  `context_present` and `effective_threshold` flow correctly into the
  router output. With bump=0, routing is unchanged WITH vs WITHOUT.
- Test 5 (rescoped to routing-decision-only): **PASS** — 6/6 baseline
  inputs route to their expected category (Code/Writing/Generic/Decline).
- Tests 2 and 3 (Mode A stack fill-in, Mode B file-naming): deferred to
  manual LLM eval — harness prints procedure. Acceptance verification
  via the user's hand-run hotkey sessions.

`assemble_context_block` was promoted from `pub(crate)` to `pub` so
the harness can call it. `pipeline` and `projects` modules promoted
from private to `pub` for the same reason.

## 5. Post-Pass-1 fixes (what changed between the two eval passes)

After running Pass 1 of the A/B harness ([eval-phase2-ab-report-pre.md](eval-phase2-ab-report-pre.md))
and getting back the user's scoring, four fixes were shipped:

1. **Tightened convention block** in all three route prompts. Now says:
   "When the context names a stack (e.g., Rust, TypeScript, Python),
   use that stack's idioms as the PRIMARY pattern in your rewrite,
   not as one option among many. Rust project → write `Result<T, E>`,
   `?`, `match`. ..."

2. **Extended Mode B NEVER** in all three route prompts. Now covers
   files/modules/functions **AND** named subsystems/feature names.
   With a new BAD example specifically about "Smart Question Engine"
   bleed.

3. **`reject_if_outsources_content` validator** in [validate.rs](../src-tauri/src/validate.rs).
   - Skip when input < 800 chars.
   - Trigger when output/input ratio < 0.33 AND 2+ outsourcing phrases
     match. Both conditions required — terseness alone is fine; phrase
     presence alone is fine.
   - Phrase list (9): `the specified`, `the listed`, `the above`,
     `the requirements`, `the constraints`, `as described`,
     `as outlined`, `the aforementioned`, `as mentioned`. Deliberately
     excludes `the existing` and `the project's` (legitimate grounding).
   - 4 new boundary tests.
   - Wired into `validate_and_repair` rejection phase, after
     `reject_if_too_long` (per brief).
   - `ValidatorConfig` got two new fields (`min_length_ratio`,
     `min_input_chars_for_outsource`). Per-route literals in
     `pipeline.rs` + the two harnesses updated to use
     `..Default::default()` so they pick up the new fields without
     breaking the build.

4. **Generic route temperature 0.4 → 0.3** in
   [pipeline.rs](../src-tauri/src/pipeline.rs) and synced in both
   eval harnesses' `knobs_for`. Code (0.3) and Writing (0.6)
   unchanged. The 0.4→0.3 drop is justified by Pass 1's observed
   1.9× variance on identical Generic-route inputs (647/1240/1105
   chars on the favorites prompt).

All four fixes are in the working tree on top of Phase 2 Steps 1-10.

## 6. Eval state — Pass 2 never landed clean

- Pass 1 (`eval-phase2-ab-report-pre.md`) — 8 outputs (6 favorites +
  2 bug-report) — landed cleanly, scored by user.
- Pass 2 (`eval-phase2-ab-report-post.md`) — 14 outputs (6 favorites
  + 6 of new Prompt 1.5 refactor + 2 bug-report) — attempted twice,
  both attempts ate Groq's 100,000-token daily budget mid-run.
  The post-fix report exists but **its diff section is partially
  polluted** because 4 of the 14 calls fell back to input verbatim
  on 429s.

**Trustworthy signals from Pass 2 despite the pollution:**

- **Prompt 1 routed to Generic, not Code.** Recontextualised the
  Mode A complaint — OUTPUT 1's "Rust as 'consider'" softness was
  the Generic prompt's softer mission language, not Code failing.
- **Prompt 1.5 routed to Code** as designed.
- **Mode A signal: 0 Rust-idiom hits** in 3 WITH-context Code-routed
  outputs. The tightened convention block didn't push the model
  toward `Result<T, E>` / `?` / `match`. **Mode A fix did not take.**
- **Mode B signal: 0 "Smart Question Engine" leaks**, **2 "PromptForge"
  leaks.** The extended NEVER suppressed the subsystem name but the
  model still volunteered the product name. The Mode B fix
  partially took but missed the project-name case — likely because
  the project name is the `Project:` line of the context bundle,
  which the LLM treats as authoritative scope.
- **Generic variance: 593 chars (pass 1) → 106 chars (pass 2,
  cleanly).** The temperature drop produced a clear, measurable
  improvement.
- **Validator firings: 0.** `reject_if_outsources_content` did not
  fire on any of the 14 runs — none of the outputs were both
  compressed below 0.33 AND carrying 2+ outsourcing phrases.

**Open: Pass 3 (clean re-run) is blocked on one of:**

- (a) UTC midnight passes and the per-org Groq TPD budget resets, OR
- (b) User signs up with a different Groq email (different org_id,
  fresh 100K/day), OR
- (c) User upgrades to Groq Dev tier (paid).

Without (a/b/c) the harness can't produce a clean 14-output Pass 3.
And without Pass 3 we don't have hard evidence on whether the Mode A
wording fix would take with cleaner sampling.

## 7. Beta-badge UI pass (separate from eval)

Mid-session UI task. Added a "Beta" pill to the Projects feature:

- **Sidebar** ([Shell.tsx:162-172](../src/components/Shell.tsx#L162-L172)):
  small pill at the right end of the Projects nav-item row.
  `.pf-beta-badge` styling in [Shell.css](../src/components/Shell.css)
  — rounded pill, `var(--pf-accent-soft)` background,
  `var(--pf-accent)` text, 9.5px uppercase.
- **Page header** ([ProjectManager.tsx:293-296](../src/components/ProjectManager.tsx#L293-L296)):
  optically larger pill (11px) next to the "Project Context" h1.
  `.pm-title-beta` styling in [ProjectManager.css](../src/components/ProjectManager.css).

Zero functional change — Projects works identically. Visible only.

## 8. Rate-limit notification feature

Latest feature shipped, currently at a decision-point. Verified
working through the trace + dev log; missing only a real OS-toast
mechanism.

### What's done

- **`GroqError` typed enum** in
  [enhance.rs](../src-tauri/src/enhance.rs):
  `RateLimit { message }`, `Network(String)`, `InvalidResponse(String)`,
  `Other { status, body }`. With Display + Error impls.
- **Pure `classify_response(status, body)`** does HTTP-status +
  body-marker rate-limit detection. Both signals are checked
  (`status == 429` OR `body.contains(r#""code":"rate_limit_exceeded""#)`).
  Defensive against Groq ever switching to HTTP 200 + error-body, even
  though they don't today.
- **`call_llm` returns `Result<String, GroqError>`** instead of
  `anyhow::Result<String>`. Single call site (`pipeline::run`) updated.
- **Pipeline Stage D** dispatches via `classify_llm_error(&e) ->
  (validation_outcome, reject_reason)`. RateLimit →
  `("fallback_rate_limit", "groq_rate_limit")`. Other →
  `("n/a", "llm_error: ...")`.
- **`friendly_reason("groq_rate_limit")`** returns the exact toast
  text `"Groq API rate limit reached. Try again in a moment."`
- **`tray::notify_rate_limit(app)`** sibling function added, plus
  `pub const RATE_LIMIT_MESSAGE`. Currently `#[allow(dead_code)]`
  because the live flow uses the indirect `friendly_reason` →
  `notify_fallback` path to avoid double-toast risk; the sibling
  exists per the brief's API requirement.
- **10 new tests.** 6 in `enhance::tests` for `classify_response`,
  4 in `pipeline::tests` for `classify_llm_error` + the new
  `friendly_reason` mappings. All pass.

### Verified working end-to-end

From today's dev log (`bug46x0qy.output`, post-restart):

```
[pipeline] LLM call failed (fallback_rate_limit): groq rate limit: {"error":{"message":"Rate limit reached for model `llama-3.3-70b-versatile` ..."}}
[replace]   SendInput delivered 4 events successfully
[fallback] Groq API rate limit reached. Try again in a moment.
[latency] silent path hotkey→pasted=489ms (enhance=216ms paste=140ms) route=writing fallback=true
```

5 hotkey-triggered rate-limit hits, 5 correct `[fallback]` log
lines, 5 trace records with `"validation_outcome":"fallback_rate_limit"`
and `"reject_reason":"groq_rate_limit"`. Detection / classification /
fallback path / trace are all correct.

### The open problem

**No visible OS notification fires.** `notify_fallback` updates the
tray-icon tooltip for 1.5 seconds, which is only visible if the user
happens to be hovering on the tray icon during that exact window.
This matches Phase 1's `migration-report.md` §6.5 + §7.1 known gap —
a real toast requires `tauri-plugin-notification` (not currently a
dep) or a frontend listener (not built).

### Three options on the table (user has not yet picked)

| Option | What you get | Diff size | Notes |
|---|---|---|---|
| **A. `tauri-plugin-notification`** (recommended) | Real Windows Action Center toast. Visible top-right, persists in action center. | ~50 lines | New Tauri-official dep |
| **B. Frontend toast** | Banner inside open PromptForge window, listens to existing `pipeline:fallback` event | ~80 lines | Useless when no PF window is foregrounded |
| **C. Always-on toast window** | Tiny Tauri window appears bottom-right ~3s | ~170 lines | Most code, most flexible |

Phase 1's reasoning for the surrogate-only approach was "the brief
disallows new dependencies." The user's rate-limit-notification
brief effectively lifts that constraint. Option A is the right
answer.

## 9. Active runtime state right now

| Resource | Value |
|---|---|
| Dev server task ID | `bug46x0qy` (running, hotkey registered) |
| Source-of-truth working tree | `C:\Users\haris\Projects\prompt-main` |
| Current branch | `phase-1-migration` (Phase 2 work is uncommitted on top) |
| Git remote origin | `https://github.com/Lakshmiyadav65/prompt.git` |
| Auth identity for push | The user has push access to Lakshmiyadav65/prompt confirmed |
| API key location | `%APPDATA%\com.promptforge.app\settings.json` (key prefix `gsk_Bvl2Q6...`) |
| API key org | `org_01kret3kcce0rsj8fx3b2g7hbj` — daily TPD shared across all keys from this Groq account |
| Today's trace file | `%APPDATA%\com.promptforge.app\traces\2026-05-15.jsonl` |
| Today's TPD state | ~99,500 / 100,000 used. Resets at UTC 00:00 |
| Active project | id `proj_1778767636512`, name `prompt`, description = PromptForge README dump (1530 chars), path = null, github link present |

## 10. Files changed since Phase 1 snapshot

`git diff --stat HEAD`:

```
 prompts/code-enhancer.md           |  28 ++
 prompts/generic-enhancer.md        |  28 ++
 prompts/writing-enhancer.md        |  28 ++
 src-tauri/examples/eval_pass2.rs   |  11 +-
 src-tauri/src/clarify.rs           |   1 -
 src-tauri/src/developer_enhance.rs |  52 ---  (deleted)
 src-tauri/src/enhance.rs           | 200 ++++++++-
 src-tauri/src/github_analyze.rs    | 169 ++++++-
 src-tauri/src/hotkey.rs            |  61 +--
 src-tauri/src/lib.rs               |   9 +-
 src-tauri/src/pipeline.rs          | 654 +++++++++++++++++++++++++--
 src-tauri/src/project_scan.rs      | 880 +++++++++++++++++++++++++++++++------
 src-tauri/src/projects.rs          | 149 +++++++
 src-tauri/src/router.rs            | 160 ++++++-
 src-tauri/src/trace.rs             |  48 +-
 src-tauri/src/tray.rs              |  30 ++
 src-tauri/src/validate.rs          | 161 +++++++
 src/components/ProjectManager.css  |  46 ++
 src/components/ProjectManager.tsx  |  86 +++-
 src/components/Shell.css           |  20 +
 src/components/Shell.tsx           |   3 +
 21 files changed, 2511 insertions(+), 313 deletions(-)
```

Plus 4 untracked files:

```
?? docs/eval-phase2-ab-report-post.md     (post-fixes A/B report — partially polluted by 429s)
?? docs/eval-phase2-ab-report-pre.md      (pre-fixes A/B report — clean baseline)
?? src-tauri/examples/eval_phase2_ab.rs   (A/B harness for context-bleed diagnosis)
?? src-tauri/examples/eval_phase2_context.rs   (Step 10 acceptance harness)
```

## 11. Recommended next actions, in order

1. **Commit Phase 2 to a `phase-2-context` branch and push.** The
   work is too far along to live in working-tree-only state. Suggested:
   ```
   git -C C:\Users\haris\Projects\prompt-main checkout -b phase-2-context
   git -C ... add -A
   git -C ... commit -m "Phase 2 — project-context-aware enhancement (+ rate-limit notification scaffolding)"
   git -C ... push -u origin phase-2-context
   ```

2. **Pick A/B/C for the rate-limit notification.** This is the
   smallest open user-facing decision. Option A (`tauri-plugin-notification`)
   is the recommended path. Concrete steps for A:
   - Add `tauri-plugin-notification = "2"` to `src-tauri/Cargo.toml`
   - Add `notification:default` to `src-tauri/capabilities/default.json`
   - `.plugin(tauri_plugin_notification::init())` in
     [lib.rs](../src-tauri/src/lib.rs)'s builder chain
   - Modify `tray::notify_fallback` to also fire `Notification::new(...)`
     alongside the existing tooltip-update surrogate
   - First-run permission prompt fires automatically; user clicks Allow
   - Manual verification: hotkey-trigger a rate limit, watch a Windows
     toast appear top-right

3. **When the Groq TPD budget is restored** (UTC midnight, fresh
   account, or paid Dev tier): re-run Pass 3 of the A/B harness.
   ```
   cargo run --manifest-path C:\Users\haris\Projects\prompt-main\src-tauri\Cargo.toml --example eval_phase2_ab
   ```
   This re-writes `docs/eval-phase2-ab-report-post.md` with clean
   numbers. Diff section automatically compares against the
   embedded Pass-1 baseline.

4. **Address the lingering Mode A weakness.** Pass 2's clean signal
   was: 0 Rust-idiom hits in WITH-context Code-routed outputs even
   after the convention block was tightened. The "PRIMARY pattern"
   wording is not strong enough. Two likely next iterations:
   - Stronger imperative wording: replace "use that stack's idioms as
     the PRIMARY pattern" with "rewrite using only that stack's
     idioms" + a NEVER specifically forbidding hedging
     ("Never present multiple stacks as alternatives — the project's
     stack is the only choice.")
   - Or: re-tune the Code-route system prompt so its existing "Never
     invent facts" NEVER doesn't compete with "use the project's
     stack idioms" (the model may be resolving tension by hedging)

5. **Address the lingering Mode B weakness.** Pass 2's clean signal
   was: 0 SQE leaks, but 2 "PromptForge" leaks. The product-name
   leak path isn't covered by the Mode B NEVER. Options:
   - Add a third BAD example specifically about product names:
     "input `fix the bug` + context project name "PromptForge" →
     rewrite reframes as "fix the bug in PromptForge..." (NEVER)"
   - Or: omit `Project: {name}` from the bundle entirely when the
     input doesn't reference the project by name. More invasive but
     cleanly cuts the leak source.

6. **Retire `Phase 2.5` placeholder.** The `CONTEXT_THRESHOLD_BUMP = 0`
   with `TODO(Phase 2.5)` comment in [router.rs](../src-tauri/src/router.rs)
   was the right call given the calibration data. Picking it up
   requires re-tuning `score_ambiguity` so inputs land in the 70-84
   band where Mode D would actually unlock a route. Defer until
   there's user demand.

## 12. Gotchas worth knowing

- **The harness's `--manifest-path` flag is required.** The
  PowerShell tool's harness resets `cwd` between commands. Don't
  rely on `cd`; pass `--manifest-path` to cargo and absolute paths
  to Read/Edit/Write.
- **Smart App Control transiently blocks freshly-built test
  binaries.** Symptom: `error: test failed ... could not execute
  process ... An Application Control policy has blocked this file.
  (os error 4551)`. Workaround: delete `target/debug/deps/promptforge_lib-*`
  and re-build; the fresh binary content gets a different hash and
  AppControl re-evaluates.
- **Em-dash encoding mismatch.** When the harness's `format!()`
  string literals use em-dash characters AND a filter elsewhere uses
  em-dash characters, they're identical UTF-8 bytes (proper) AS LONG AS
  both come from the same Edit operation. If one was written by Write
  and the other by Edit (or by a PowerShell re-save), encoding can
  drift. Filter on ASCII-only prefixes (`l.starts_with("Prompt 1 ")` +
  `l.contains("WITHOUT")`) to be safe.
- **Groq's TPD limit is per-organization, not per-key.** Creating a
  new API key from the same Groq account doesn't reset the daily
  budget. User confirmed: the `gsk_Bvl2Q6...` key is from the same
  org as the previously-used `gsk_ZV2h...` key — both share the
  100K/day bucket. Fix is a different account, or upgrade to Dev tier.
- **`notify_fallback` is not a real toast.** Tray-tooltip surrogate.
  See §8 for the fix-options.
- **The dev server doesn't pick up Rust changes via hot reload.**
  Vite hot-reloads the frontend (TSX/CSS), but `cargo run --no-default-features`
  only invokes cargo once. Rust changes require killing the dev
  process (`TaskStop bug46x0qy`) and re-running `npm run tauri dev`.
- **`describe_context_block` truncation marker.** When the bundle
  exceeds 2000 chars, readme is truncated first, then file_layout,
  then core (last resort). The marker is `\n... [truncated]`. If
  you see that in a context bundle, the project description plus
  scan output blew past 2000 chars — usually because the description
  is a github-analyze README dump.

---

This handoff was written 2026-05-15 at the close of a multi-day
collaboration. If anything in it conflicts with the actual code
state, **trust the code**, not the handoff — the working tree is
the source of truth.
