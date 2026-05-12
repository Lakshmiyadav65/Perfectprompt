# Smart Question Engine — Quality Eval Protocol

PRD §13 Q3 calls for an empirical validation of the claim that the question
card produces better prompts than the silent path. This document defines the
protocol so that claim can be tested before V1 ship — and re-tested whenever
the meta-prompt, scorer, or static bank changes.

## What we're measuring

For each captured input, we get **two outputs**:

- **A — silent path**: `Ctrl+Alt+E` with `question_mode = Silent` (or
  `Shift+Ctrl+Alt+E` on any setting). One LLM call, no clarifying questions.
- **B — card path**: `Ctrl+Alt+E` with `question_mode = Always ask`. Pick the
  most defensible answer for each chip, then "Enhance Now".

We rate each output on three axes and report the win rate / mean delta for
the card path over the silent baseline.

## Sample inputs (30)

Stratified across the six domains the engine routes to. Each row is one
captured selection. Use exactly the text in the **Input** column.

### Coding (5)

| #  | Input                                                                   |
| -- | ----------------------------------------------------------------------- |
| 1  | `fix the bug`                                                           |
| 2  | `write a function to dedupe a list while preserving order`              |
| 3  | `make the dashboard load faster`                                        |
| 4  | `add tests`                                                             |
| 5  | `refactor this`                                                         |

### Email (5)

| #  | Input                                                                   |
| -- | ----------------------------------------------------------------------- |
| 6  | `write a leave mail`                                                    |
| 7  | `reply to john`                                                         |
| 8  | `email about the deadline slip`                                         |
| 9  | `say no to the meeting request`                                         |
| 10 | `follow up on the invoice`                                              |

### Writing (5)

| #  | Input                                                                   |
| -- | ----------------------------------------------------------------------- |
| 11 | `write a blog post on serverless`                                       |
| 12 | `draft a post announcing the v2 launch`                                 |
| 13 | `write copy for the pricing page`                                       |
| 14 | `essay on remote work`                                                  |
| 15 | `article about LLM prompt engineering`                                  |

### Research (5)

| #  | Input                                                                   |
| -- | ----------------------------------------------------------------------- |
| 16 | `what is observability`                                                 |
| 17 | `explain how DNS resolves CNAME records`                                |
| 18 | `summarise this paper`                                                  |
| 19 | `research the state of WebGPU support`                                  |
| 20 | `how does TLS 1.3 differ from 1.2`                                      |

### Analysis (5)

| #  | Input                                                                   |
| -- | ----------------------------------------------------------------------- |
| 21 | `compare React and Solid`                                               |
| 22 | `analyse our Q3 churn`                                                  |
| 23 | `review this design doc`                                                |
| 24 | `evaluate options for the message queue`                                |
| 25 | `assess the security of this endpoint`                                  |

### Generic (5)

| #  | Input                                                                   |
| -- | ----------------------------------------------------------------------- |
| 26 | `tell me a joke`                                                        |
| 27 | `pitch the idea`                                                        |
| 28 | `make a plan`                                                           |
| 29 | `give feedback`                                                         |
| 30 | `outline the proposal`                                                  |

## Running the eval

1. **Set up two profiles** so you can toggle between paths quickly:
   - Open `Tray → Settings → Smart Question Engine`. Note the current mode so
     you can restore it after.
2. **For each input above:**
   - Type or paste the input into Notepad / VS Code / any editor, select it.
   - **Silent run:** set `question_mode = Silent`, press `Ctrl+Alt+E`. Copy
     the enhanced output into row A of your results sheet.
   - **Card run:** set `question_mode = Always ask`, press `Ctrl+Alt+E`. Pick
     the most defensible answer for each question (e.g. the first option that
     a reasonable user would pick for this input), click **Enhance Now**.
     Copy the enhanced output into row B.
3. **Watch the dev console** during the card run for two log lines:
   - `[latency] card path hotkey→shown=Nms` — should be < 600ms (PRD §12).
   - `[latency] fetch_question_card_session ready in Nms (source=...)` —
     should be < 3000ms.
   Record both into the latency columns.

## Rating rubric

Rate each enhanced output (A and B) on each axis, 1 to 5:

### Specificity (does it tell the agent **what to do** concretely?)

- **5** — Concrete subject + concrete verb + concrete output shape. A senior
  engineer / writer would not need to ask follow-up questions before acting.
- **4** — Mostly concrete; one minor gap a reasonable agent would resolve.
- **3** — Roughly directional. Agent would have to guess on ~2 dimensions.
- **2** — Vague. Agent has to invent most of the requirements.
- **1** — Indistinguishable from the original rough input.

### Constraint adherence (does it respect the captured / answered context?)

For silent runs, "captured context" = anything in the input itself.
For card runs, "captured context" = input + the answers you provided.

- **5** — Every constraint expressed in the input/answers is reflected in
  the output. No contradictions.
- **4** — All constraints present; one is loosely interpreted.
- **3** — One constraint is missed or weakened.
- **2** — Multiple constraints missed or contradicted.
- **1** — Output ignores the constraints entirely.

For card runs, also check **no leakage**: the output must not echo the
`[CONTEXT]` block, the `Original input:` label, or the impact-dimension list.
Any leakage caps this score at 2 regardless of adherence.

### Length appropriateness (does it match the input's weight?)

- **5** — 2–4 sentence rewrite for a one-line input; complex inputs keep
  their complexity. No padding.
- **4** — Slightly long or slightly short but still readable.
- **3** — Noticeably padded or noticeably truncated.
- **2** — A full paragraph for "fix the bug"; or one terse sentence for a
  multi-constraint email.
- **1** — Length is wildly off (e.g. five paragraphs for "tell me a joke").

## Results template

Copy this table into a spreadsheet or markdown file and fill it in:

| #  | Domain    | A.spec | A.con | A.len | B.spec | B.con | B.len | Card faster path? | shown_ms | ready_ms | Notes |
| -- | --------- | ------ | ----- | ----- | ------ | ----- | ----- | ----------------- | -------- | -------- | ----- |
| 1  | Coding    |        |       |       |        |       |       |                   |          |          |       |
| 2  | Coding    |        |       |       |        |       |       |                   |          |          |       |
| …  | …         |        |       |       |        |       |       |                   |          |          |       |

## Acceptance criteria

The card path passes the PRD Q3 bar if, across the 30 inputs:

| Metric                                            | V1 target          | Source         |
| ------------------------------------------------- | ------------------ | -------------- |
| Mean B − A across the three axes                  | > +0.5 per axis    | PRD §13 Q3     |
| % inputs where B beats A on at least one axis     | > 70%              | PRD §12        |
| % runs where card-shown latency is < 600ms p95    | > 95%              | PRD §12        |
| % runs where questions-ready latency is < 3000ms  | 100% (hard cap)    | PRD §6.5       |
| Leakage rate (CONTEXT block echoed in output)     | 0                  | PRD §8 (Add 2) |

If any row fails, the meta-prompt or the question-generation prompt is the
first place to investigate. Re-run the eval after changes.

## Cost

At Groq's free tier (~30 req/min on `llama-3.3-70b-versatile`), 30 inputs
× 2 runs = 60 enhancement calls plus 30 question-generation calls (small
model) ≈ 5–10 minutes of API time. Stays well under the free-tier limit.
