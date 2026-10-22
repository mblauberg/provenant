# Spec: replay bench studies, model-effort frontier and fleet topology

Status: **proposed**, 2026-10-22, for issue #1064. This file defines two
studies that run on the [replay bench](replay-bench.md). It reuses that
spec's statistics (unit, repeats, win rule, guards, power, zero cases),
sets and split rules (B1), isolation and confidentiality (B2), run rules
(B3) and row provenance (B4) by reference. It grants no authority, and a
config change it proposes lands only through its own PR.

## Shared rules

- **Cost.** We pay by subscription, so the primary cost is the quota share
  per account window: the `used_percent` delta over a block, divided by the
  block's accepted changes, reported for each window (5-hour and weekly).
  Weighted tokens (main spec B4.4) and listed USD are secondary.
  - Across providers, the table also shows the plan-dollar equivalent of
    each share. For example, a 1% weekly share on a USD 100/month plan is
    about USD 0.23.
- **Quota blocks.** Quota attribution follows main spec B4.5.
  - Each account runs one cell at a time, in alternating ABAB blocks of
    whole tasks. These blocks replace main spec B3.4's per-attempt
    interleaving for that account.
  - A block is void if the ledger shows other sessions on that account
    during it.
  - A block must move the meter by at least 10 points, so that the meter's
    1-point resolution stays below 10% error.
- **Confidentiality.** No confidential or Sealle task is sent to a model
  whose catalogue entry is `trains_on_prompts: true` or unset; unset counts
  as true. Main spec B2.6 applies to every Sealle artefact.
- **Rows.** Every row adds `cell` or `shape`, plus the family mix per tier,
  to the main spec B4.2 fields.

## Study A: model x effort frontier per task class

### Grid

| Family | Cells |
|---|---|
| Claude | `claude-haiku-5-5` low, medium, high; `claude-sonnet-5-5` medium, high; `claude-opus-5-5` low, medium, high; each model at xhigh as a probe |
| OpenAI | `gpt-6-luna` and `gpt-6.1-sol`, low, medium, high, xhigh |
| OpenCode | `opencode-go/deepseek-v4.1-flash` at its catalogue default |
| Cursor | `grok-4.7` at its route default |

That gives 21 cells. Sonnet 5.5 has no `low` in the catalogue, so its low
cell does not exist and is not substituted. The xhigh probes run in Stage 1
only, and they enter Stage 2 only if they are not dominated.

### Tasks and acceptance

- **Task classes.** Replay tasks are classed by size as `mechanical`,
  `legwork` and `implementation` (operating model R1.2), and the class is
  frozen at mining. Review, research and UI classes are out of scope here.
- **Isolated writer.** Study A measures the writer cell alone:
  - each attempt is one writer lane with tier review disabled;
  - an accepted change is a lane `ok` with both oracles passing;
  - cost covers only that lane.

  This deviates from main spec B3.6 on purpose. A review ladder would add
  the reviewer's model to every cell.
- **Effort floors.** Cells pin both model and effort, overriding route
  floors for the experiment only. `implementation`'s high floor is policy,
  so a proposed default below it must change the floor in the same PR.

### Stages

1. **Stage 1, screen.** Search set (20 tasks), 1 repeat, all cells, 420
   attempts.
   - A cell is pruned when another cell beats it by more than two pooled
     standard errors (main spec Stats 9) on both cost per accepted change
     and acceptance rate. Pruning is pooled across classes, because 20
     tasks cannot screen each class. A cell whose blocks move the meter by
     less than 10 points screens on weighted tokens instead, and the
     report flags it.
   - The cells the questions below need are protected from pruning:
     Haiku high, Opus low and medium, every Sonnet cell, Luna xhigh, and
     Sol low and medium.
2. **Stage 2, decide.** Acceptance set, 3 repeats, surviving cells only.
   - A class needs at least 30 acceptance tasks before its table can
     decide. Mining tops up by class (main spec B1.5).
   - A class below 30 tasks is reported `underpowered`, and its table is
     descriptive only.

### Questions it must answer

Each is a paired comparison under the main spec's win rule, with the claim
pre-registered.

1. Does Haiku at high, or Opus at low or medium, beat Sonnet's best cell in
   a class? "Beat" means an efficiency win with the pass@1 guard holding,
   or a capability win with the quota-share guard holding.
2. Where is a Sonnet or Sol cell uniquely on the frontier, meaning no other
   model's cell is within the screen on both axes? Reported per class with
   the margin to the nearest alternative.
3. Does Luna at xhigh dominate Sol at low or medium: no worse on either
   axis beyond the guard tolerance, and a win on at least one?

### Output

- **Pareto table per class:** cell; pass@1 with its interval; pass^3; quota
  share per accepted change, per window; weighted tokens per accepted
  change; median wall time; timeout and empty-patch rates; frontier flag;
  and the answers above.
- **Proposed `role_effort_defaults` diff** for `config/model-routing.json`.
  Today only `families.openai` has the block (worker: workhorse high, scout
  high); `families.anthropic` would gain one. Any
  `task_class_routes.<class>.effort` change is listed beside it. Each
  changed entry cites the comparison that won it; entries without a win
  stay unchanged.

### Budget

- Stage 1 costs about 420 x m tokens, where m is the median writer lane.
  At the ledger's 1.28M median lane that is about 540M tokens, about 97%
  of them cache reads.
- Stage 2 costs C x T x 3 x m, for C surviving cells and T acceptance tasks.
- The quota share per block is measured, not estimated.

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
- No pruning per class from 20 tasks.
- No fleet arm scored by a judge from its own principal family.
- No Sealle or confidential task on a model that trains on prompts.
- No routing config change without a winning comparison.
