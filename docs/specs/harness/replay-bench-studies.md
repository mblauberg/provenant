# Spec: replay bench studies, model-effort frontier and fleet topology

Status: **proposed**, 2026-10-22, for issue #1064. This file defines two
studies that build on the [replay bench](../../runbooks/bench.md). It reuses that
spec's statistics (unit, repeats, win rule, guards, power, zero cases),
sets and split rules (B1), isolation and confidentiality (B2), run rules
(B3) and row provenance (B4) by reference. It grants no authority, and a
config change it proposes lands only through its own PR.

## Shared rules

- **Cost.** We pay by subscription, so in dedicated runs (Study A Stage 2,
  Study B) the primary cost is quota share per account window (5-hour and
  weekly) per accepted change, with its plan-dollar equivalent across
  providers; 1% of a weekly window on a USD 100/month plan is about USD
  0.23. Weighted tokens (main spec B4.4) and listed USD are secondary.
- **Quota blocks** (dedicated runs, main spec B4.5). Each account runs one
  cell at a time in alternating ABAB blocks of whole tasks, which replace
  B3.4's per-attempt interleaving for that account. A block is void if the
  ledger shows other sessions on the account, and it must move the meter
  by at least 10 points, keeping 1-point meter resolution under 10% error.
- **Confidentiality.** No confidential or Sealle task is sent to a model
  whose catalogue entry is `trains_on_prompts: true` or unset; unset counts
  as true. Main spec B2.6 applies to every Sealle artefact.
- **Rows.** Every row adds `cell` or `shape`, plus the family mix per tier,
  to the main spec B4.2 fields.

## Study A: model x effort frontier per task class

Study A starts from published results and learns from real work. The
dedicated replay grid is a fallback, not the main instrument.

### Candidate grid

| Family | Cells |
|---|---|
| Claude | `claude-haiku-5-5` low, medium, high; `claude-sonnet-5-5` medium, high; `claude-opus-5-5` low, medium, high; xhigh on each as a probe only |
| OpenAI | `gpt-6-luna` and `gpt-6.1-sol`, low, medium, high, xhigh |
| OpenCode | `opencode-go/deepseek-v4.1-flash` at its catalogue default |
| Cursor | `grok-4.7` at its route default |

Sonnet 5.5 has no `low` in the catalogue, so that cell does not exist.
Classes in scope are the writer classes `mechanical`, `legwork` and
`implementation`; review, research and UI classes are out of scope.

### Stage 0: external priors

1. A research lane collects published per-effort results into
   `docs/research/effort-priors.md`, seeded from E1: Artificial Analysis,
   vendor effort curves, SWE-bench Pro, Terminal-Bench 2.x and similar.
   Each row records model, effort, benchmark, score, cost or tokens, source
   URL, publication date and confidence (high, medium, low, rated as in
   R1-R3), and the class the benchmark maps to (for example SWE-bench Pro
   to `implementation`).
2. Priors only order the work. A cell that another cell of the same family
   beats on both score and cost, beyond the source's reported noise (else
   5 points and 20%), in at least two medium-or-higher sources with no
   contrary source, is marked `externally_dominated`. It gets no Stage 2
   grid spend. It stays available as a Stage 1 challenger, queued after
   the undominated cells. The marks and their reasons are recorded, and
   the table is refreshed when a model enters the catalogue.
3. Public leaderboards are contaminated (main spec Not doing), so a prior
   never changes a default and is never acceptance evidence. Every routing
   change needs an internal win.

### Stage 1: in-flight arm on real work

1. **Eligibility.** A lane routed by default (no explicit model or
   `principal:` override), in an in-scope class, at `routine` or
   `substantial` risk, and not confidential unless neither cell's model
   trains on prompts (unset counts as training).
2. **Trials.** Each class runs one pre-registered trial at a time: its
   current default cell against one adjacent challenger. Adjacent means
   either the same model at one effort step up or down, or the
   neighbouring tier at the same effort. Route floors still apply on real
   work.
3. **Assignment.** At route resolution, before any outcome is seen, a
   seeded hash of the lane id sends the lane to the challenger with
   probability `routing.inflight_share`. The share is a proposed #1070
   knob, an int percent from 0 to 25, default 10; 0 turns trials off.
   Because assignment is random, task difficulty balances between arms in
   expectation, which #1056's before/after comparison cannot do.
4. **Telemetry.** Each lane records `routing.trial_id`, `routing.arm`
   (`default` or `challenger`), `routing.assignment_p` and an ineligibility
   reason. Fabric receipts carry these fields, and the ledger ingests them.
   Native spawns join once the R1.4 soft gate stamps the assignment.
5. **Outcomes.** The primary estimand is frozen in the trial record before
   the first assignment. Quota cannot be attributed to a lane in flight,
   so the primary is cost per accepted change (operating model R4.3) in
   weighted tokens, with acceptance as its guard. Apportioned quota share
   (flagged approximate), tokens, wall-clock time and review findings (raw
   counts until per-finding dispositions exist, MAP "Missing") are
   descriptive and never decide a trial.
6. **Analysis.** The main spec's statistics apply, except that tasks are
   not paired: the bootstrap is stratified by class, risk tier, size and
   repository and clustered on task, and a within-stratum re-randomisation
   test of the assignment replaces the sign test as the agreeing check.
7. **Budgets,** pre-registered per trial. A challenger lane is capped at 2x
   the class's default-cell median weighted tokens; at the cap it stops,
   its task reroutes to the default, and it counts as a challenger failure
   (intention to treat). The challenger's cumulative excess over the
   default median is capped, by default at 5% of the class's eligible
   weighted tokens per week, checked at every lane completion; reaching it
   stops the trial as `stopped_budget`. These caps, not the look schedule,
   bound overspend.
8. **Stopping.** Guardrails are checked at every challenger completion: the
   challenger's one-sided acceptance bound worse than the pass@1 tolerance,
   or its cost per accepted change more than 50% above the default's.
   Verdicts are read once, at the sample size set by the power simulation
   (main spec Stats 10):
   - `win` follows the main spec's rule;
   - `within_noise` is two one-sided tests at alpha 0.05 against a
     pre-registered margin, by default +/-10% on cost per accepted change
     (#1056's noise band) and the pass@1 tolerance on acceptance. Both
     90% bounds must lie inside the margin;
   - otherwise the trial is `no-win`;
   - after N weeks (default 6) without the planned sample, it is
     `inconclusive`.

### Stage 2: optional confirmation on the replay bench

This stage runs only for a class where Stage 0 and Stage 1 disagree beyond
noise, or where Stage 1 is still `inconclusive` after N weeks. It also
answers comparisons that are not adjacent, such as Luna at xhigh against
Sol at low or medium.

Each attempt is a single writer lane with tier review off, costed for that
lane only. This deviates from main spec B3.6 deliberately, so that the
reviewer's model does not confound the cell. Cells pin model and effort,
overriding route floors for the experiment only. The search set runs at 1
repeat; a cell is pruned when another beats it on both cost per accepted
change and acceptance rate by more than two pooled standard errors, pooled
across classes, with the cells the questions need protected. The
acceptance set then runs at 3 repeats; a class with fewer than 30 tasks is
`underpowered` and descriptive only.

### Questions

Each answer cites the stage that settled it. (1) Does Haiku at high, or
Opus at low or medium, beat Sonnet in a class? Stage 1 runs the adjacent
pairs (Haiku high against Sonnet high, Opus medium against Sonnet medium);
Stage 2 takes the rest. (2) Where is a Sonnet or Sol cell uniquely on the
frontier, with no other model's cell within noise on both axes? (3) Does
Luna at xhigh dominate Sol at low or medium, no worse on either axis beyond
tolerance and better on one? Stage 0 orders it; Stage 2 decides it.

### Output and budget

- **Pareto table per class:** cell, pass@1 with its interval, pass^3
  (Stage 2 only), quota share and weighted tokens per accepted change,
  median wall time, frontier flag, and the source stage of each figure.
- **`role_effort_defaults` diff** for `config/model-routing.json`. Today
  only `families.openai` has the block; `families.anthropic` would gain
  one. `task_class_routes.<class>.effort` changes are listed beside it.
  Each entry cites its winning internal trial or Stage 2 comparison;
  priors are cited only as context. A default below `implementation`'s
  high floor must change the floor in the same PR.
- **Budget.** Stage 0 is one research lane. Stage 1 adds no lanes, since a
  challenger replaces the default, and its overspend is bounded by the
  per-lane and cumulative caps in Stage 1 item 7. At about 190
  `ok` Fabric lanes a week, a class takes weeks to reach 30 challenger
  tasks; the report projects each trial's end date. Stage 2 costs cells x
  tasks x repeats x the 1.28M median lane.

## Study B: fleet topology and ratio

### Arms

| Arm | Shape (junior:senior:principal) | Families |
|---|---|---|
| `fleet-10` | 10:3:1 | mixed, at least two families per tier where the roster allows |
| `fleet-30` | 30:6:1 | mixed, as above |
| `same-family` | 10:3:1 | one family in every tier |
| `solo` | 0:1:0 | one senior with a token budget matched to `fleet-10`'s median weighted tokens |

- Fleets run the bottom-up tiered council from `orchestrate`: juniors
  sweep, seniors deduplicate and verify, and the principal synthesises.
- `lanes.max_concurrency` stays at 8 for every arm.
- `fleet-30` runs only if `fleet-10` passes the Stage 1 screen against
  `solo`, because each `fleet-30` attempt is 37 lanes.

### Tasks

Frozen sets follow main spec B1.5-B1.6, stratified by kind. Tasks come
from Provenant, which is public, unless every junior meets the
confidentiality rule above.

- **Breadth, codebase sweep:** a Provenant snapshot and a sweep question.
  The findings key is frozen before the run, from past reviews whose
  findings were confirmed and dispositioned.
- **Breadth, research:** questions with a source-verified answer key, such
  as the claims V1 and V2 checked. Lanes have network access (operating
  model D14); the key files and post-cutoff research notes are on the main
  spec B2.2 deny list.
- **Design:** UI proposals and architecture options from decisions whose
  chosen option is on record (ADRs, merged UI PRs). The recorded choice is
  the user-pick oracle on a subset.

### Metrics

- **Breadth:** recall against the frozen key, and false positives that
  reach the principal (findings forwarded to the principal tier that are
  not in the key and that a blinded adjudicator rejects). Confirmed
  unkeyed findings go into key v2; recall is reported against both
  versions, and the decision uses v1.
- **Design:** a blinded, both-order pairwise preference from a cross-family
  judge panel (main spec Stats 11), whose family differs from the principal
  family of both outputs. The brief fixes the format and a word cap; length
  is recorded and its correlation with preference reported. On about 10
  pairs, user picks are the oracle; below 70% panel-user agreement the
  panel result is descriptive only. The per-task score is wins minus
  losses over repeats, and the win rule applies to it.
- **Both:** quota share, weighted tokens and wall-clock time per task. Each
  row records the shape, models and families per tier, the lane count, and
  the findings forwarded per tier.

### Stages and budget

- **Stage 1.** Search set, 1 repeat, with at least 6 breadth and 6 design
  tasks.
- **Stage 2.** Acceptance set, 3 repeats, for the arms that pass the
  screen.
- **Budget.** Tokens = tasks x repeats x (sum over arms of lanes x median
  lane).
  - At the 1.28M ledger median, a `fleet-10` attempt (14 lanes) is about
    18M tokens, and a `fleet-30` attempt is about 47M.
  - Free OpenCode juniors cost little quota, but their rate limits affect
    wall-clock time, so rate-limit events are recorded.

## Not doing

- No cross-provider ranking on USD alone.
- No pruning per class from 20 tasks, and no leaderboard result used as
  acceptance evidence.
- No in-flight randomisation on `crucial` or `terminal` work, or on
  confidential work with a model that trains on prompts.
- No fleet arm scored by a judge from its own principal family.
- No Sealle or confidential task on a model that trains on prompts.
- No routing config change without an internal win; priors only order
  trials.
