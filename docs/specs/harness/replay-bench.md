# Spec: held-out replay bench for harness changes

Status: **proposed**, 2026-10-22, for issue #1064. GitHub owns delivery
state; this file owns the durable requirements once accepted. It grants no
authority and relaxes no gate in `HARNESS.md`.

## Outcome

A harness or skill change that claims an efficiency gain is measured as a
controlled A/B on frozen tasks before it merges. The bench replays merged
Provenant and Sealle PRs: each task is a pre-change snapshot, the issue text
as the prompt, and the PR's hidden tests as the oracle. #1038 and #1056 give
an observational before/after; the bench gives the paired comparison they
cannot. It depends on the profiles and knobs in #1070 and the `harness_rev`
stamp in #1056.

## Statistics

Sources disagreed on "3+ paired repeats", "n >= 10 tasks" and "two pooled
standard errors". They answer different questions. Variance across tasks
dominates, so more tasks buy more power than more repeats (R3 F4). Repeats
cut within-task noise and are needed to measure reliability. The two-SE rule
is #1056's observational noise filter, not a test.

1. **Unit.** The task. Attempts are nested in tasks and never pooled as
   independent samples.
2. **Repeats.** 3 per task and arm. The task's pass fraction estimates
   pass@1; all 3 passing gives pass^3.
3. **Primary metric.** Cost per accepted change, C = sum of cost / sum of
   accepted over the set's tasks. The ratio of sums keeps tasks with no
   accepted attempt in the estimate.
4. **Decision.** Arms are paired by task. A stratified paired bootstrap over
   tasks (10,000 resamples, recorded seed) gives a 95% percentile CI on
   log(C_candidate / C_baseline). An exact sign test on per-task cost per
   attempt is reported alongside.
5. **Win.** Only when the CI excludes zero in the claimed direction, the sign
   test agrees in direction at p < 0.05, and no guard regresses beyond its
   tolerance. If bootstrap and sign test disagree, the verdict is
   `inconclusive`. With 30 tasks a bootstrap CI can be carried by one or two
   heavy tasks (one task supplied +0.300 of a 0.054 lead in R3 F4's source);
   the sign test is the distribution-free check against that, not a
   tie-break that can turn a non-win into a win.
6. **Guards.** pass@1, pass^3, median wall time, attention events,
   empty-patch rate and timeout rate, each with a paired bootstrap CI on the
   task-level difference. A guard fails if its point estimate is worse than
   its tolerance, or its CI excludes zero in the worse direction. Exact
   McNemar on per-task pass^3 is reported for discordant tasks.
7. **Tolerances** are frozen in the plan. Defaults: pass@1 and pass^3 -0.05
   absolute; median wall time +25%; attention events per task +10%;
   empty-patch and timeout rates +0.03 absolute. A plan may tighten a
   tolerance, never loosen one. `harness-knobs.json` carries no tolerances,
   so the plan owns them.
8. **Claim direction.** A plan names its claim. For an efficiency claim (the
   #1064 default) cost is primary and pass@1 a guard. For a capability claim
   such as `rigorous`, pass@1 is primary and cost becomes a guard with a
   stated ceiling, for example +50%.
9. **Search screen.** On the search set an arm advances to acceptance when
   it beats the baseline by more than two pooled standard errors (#1056's
   rule). The screen never gates a merge.
10. **Power.** The pilot measures sigma_d, the per-task standard deviation of
    the paired difference. Tasks needed at 80% power and alpha 0.05:
    n = 7.85 sigma_d^2 / delta^2, so at n = 30 the minimum detectable effect
    (MDE) is about 0.51 sigma_d. Every report states its MDE; a run sized
    below the effect it claims reports `underpowered`, not `no-win`.
11. **Oracles.** Execution only. A criterion that needs a judge follows
    `evaluate`: both orders, counted only when they agree (a disagreement is
    a tie), and the judge's family is never that of the output it scores.

## Requirements

### B1 Task mining

1. A task is a merged Provenant or Sealle PR whose added or changed tests
   fail on the parent and pass on the merge, 3 of 3 times each. The parent's
   other tests in the touched modules must pass at both ends; they form the
   pass-to-pass oracle.
2. Excluded: docs-only, dependency-bot and revert PRs; tests that need
   network, credentials or a GUI; an oracle over 10 minutes; a non-test diff
   over 400 lines; `terminal`-tier work.
3. The prompt is the closing issue's text, else the PR title and body with
   code blocks and diffs removed; the source is recorded. Tests stay hidden.
4. The risk tier comes from the PR's label, else the miner assigns it from
   `config/risk-policy.json`, and it is frozen with the task.
5. Mining targets about 100 tasks, stratified by repository and tier
   (`routine`, `substantial`, `crucial`). A seeded draw within each stratum
   assigns search (at least 20), acceptance (at least 30) and held-out (at
   least 30). Held-out favours the newest merges, so its tasks postdate
   model training cutoffs; every task records its merge date.
6. A frozen split records task ids, seed, mining `harness_rev` and its
   SHA-256 digest. A change is a new split version, never an edit.

### B2 Isolation and leakage

1. Each attempt runs in a disposable single-commit repository made from the
   parent tree, with no other refs, objects or remote, so neither `git log`
   nor the object store can reveal the fix. The writer lane gets a linked
   worktree in it under `docs/worktrees.md`.
2. Lanes run in the R9 box with GitHub denied, so they cannot read the PR or
   its issue thread.
3. Manifests, splits and per-task results live under the instance state root
   (`bench/`, beside `telemetry.sqlite3`), never in a repository: Provenant
   is public, Sealle is confidential, and a manifest committed to Provenant
   would leak held-out tasks into later tasks' snapshots. Sealle tasks run
   only on confidentiality-eligible routes (operating model D11) and their
   manifest stays on Sealle's hosts.
4. `docs/evals/replay-bench/README.md` records split versions, digests and
   release verdicts, as aggregates only.

### B3 Arms and runs

1. An arm is `{harness ref, preset or knob overrides, route}`. Its harness
   runs from a detached Provenant worktree at that ref, and the lane's
   product root resolves to it.
2. Routes are pinned so quota fallback rarely changes the model. Attempts
   count in their assigned arm (intention to treat). The report gives each
   arm's reroute rate; a gap above 10 points marks the run `invalid`.
3. The plan is an `evaluate` frozen plan (`EVALUATION.template.json`), its
   digest fixed before the first attempt; a changed plan starts a new run.
   Bootstrap and sign-test output are linked, digest-bound evidence, since
   receipt schema v2 aggregates means only.
4. A seeded schedule interleaves task, arm and repeat, so provider drift
   falls on both arms.
5. An infrastructure failure (`rate_limited`, `usage_limited`,
   `auth_required`, provider error) is retried; the retry is appended, the
   failure kept. A capability failure is never retried. Over 10% unresolved
   infrastructure failures marks the run `invalid`.
6. After the lane ends, the bench applies the merge commit's test files and
   runs both oracles. **Pass** means both pass. **Accepted** means pass and
   the arm's harness reached its done condition (lane `ok`, tier-required
   review completed). Done-but-failing attempts are reported as the
   false-done rate.
7. Held-out runs only for a release, with `--release`, and reports
   aggregates only. A held-out task inspected for tuning moves to search and
   is replaced by a newly mined one.

### B4 Ledger rows and provenance

1. Each attempt writes one JSONL row in its run directory. `provenant
   telemetry ingest` gains a manifested `bench` source and an additive
   `bench_attempts` table, which, as in #1056, needs no `user_version` bump.
2. Row fields: `bench_run_id`, `task_id`, `split`, `arm`, `repeat`,
   `preset`, `knobs` (resolved values as JSON), `harness_rev` (#1056's name,
   not `harness_git_sha`), `model_requested`, `model_resolved`,
   `model_observed`, `effort`, `route`, `fabric_run_id`, `pass`, `accepted`,
   `harness_done`, `empty_patch`, `timed_out`, `attention_events`, `wall_s`
   and `oracle_s`. `task_id` is the bench task; rows join `usage` on
   `fabric_run_id`, not on `usage.task_id`, which is the Fabric task.
3. The three model fields come from the Fabric receipt's provenance, since
   `telemetry.py` folds them into one column. `empty_patch` is computed from
   the final diff. Attention events are `input_required`,
   `permission_blocked` and escalation-to-user events.
4. **Cost** is weighted tokens: uncached input + output + reasoning +
   0.1 x cache read + 1.25 x cache write, joined from `usage`. USD from
   `config/telemetry-prices.json` is reported where a rate exists; 1,393 of
   1,414 `ok` Fabric lanes since 2026-09-01 have none, so USD cannot be the
   primary unit.
5. **Quota units** are summed `used_percent` deltas per account window over a
   block. Operating-model R4.5 makes them account-level, so they are reported
   per arm only for plans that run arms in alternating account blocks, never
   per task.
6. The report refuses a row that lacks a provenance field.

### B5 CLI

```text
provenant bench mine   --repo PATH [--since DATE] [--freeze-split SEED]
provenant bench run    --plan PLAN.json [--resume RUN] [--release]
provenant bench report RUN [--baseline ARM] [--json]
```

`mine` validates candidates (B1) and, with `--freeze-split`, writes the
split. `run` refuses held-out without `--release`. `report` prints one
verdict per arm (`win`, `no-win`, `inconclusive`, `underpowered` or
`invalid`) with the primary CI, guard table, MDE and sample sizes, within
4 KB of text.

## Pilot and budget

The pilot is an A/A test: `balanced` against `balanced` on 10 acceptance
tasks, 3 repeats each, 60 attempts. It measures run-to-run variance and
sigma_d, and checks the decision rule, since an A/A run must not yield
`win`. R1 F1 reports large rerun flips; in V1's check no Opus discordant
pair reproduced.

A read-only ledger query over the 1,414 `ok` Fabric lanes since 2026-09-01
gives a median of 1.28M tokens per lane (IQR 0.49M-3.21M), 97.6% of them
cache reads, with about 11k output and reasoning tokens. No Fabric row has
`duration_ms`, so wall time must come from receipts.

Tokens = T x A x k x L x m: tasks, arms, repeats, lanes per attempt (about
1 at `routine`, 2 at `substantial`, 3 at `crucial`, plus repairs) and the
median lane m until the pilot replaces it. With L = 2, the pilot is about
150M tokens; a two-arm comparison on 30 acceptance tasks (180 attempts) is
about 460M; a release comparison on held-out costs the same per candidate.
Weighted cost is about 0.13 x the token count, because most tokens are cache
reads. Quota units are measured in the pilot and extrapolated linearly.

## First experiments

From the open questions in R1, R2 and R3 section 3 that map to #1070 knobs,
in priority order. "Blocked" marks a dependency on telemetry MAP.md lists as
missing.

1. **A/A pilot on `balanced`** (R3 Q6). Prerequisite for the rest.
2. **`economy` against `balanced`**, efficiency claim (R3 Q5). Test the
   bundle; decompose it only on a win or a guard failure.
   `writing.final_polish` and `council.junior_size` are inert on code replay.
3. **`effort.shift` -1, 0, +1 per route**, efficiency claim (R1 Q1). The
   bench supplies `empty_patch` itself. Arms are clamped to the route floor
   (`implementation` is fixed at high; Sonnet 5.5 has no `low`), and arms
   that collapse to the same effort are dropped.
4. **`rigorous` review knobs against `balanced`**, capability claim (R3 Q1
   and Q3, R2 Q5): `review.extra_reviewers` 1, `review.cross_family`
   require, `review.rereview_scope` full, one at a time rather than
   factorial until the pilot sizes the cost. Per-finding defect recall is
   blocked on finding dispositions; the oracle outcome is not.
5. **`repair.escalate_after` 0, 1, 2 and `repair.max_cycles` 2, 3, default**
   (R1 Q2 without probes, R3 Q2). Both are advisory, so the arms test prose
   adherence. Per-cycle attribution is blocked on escalation links and the
   per-cycle failing-check set.
6. **`routing.pool_mode` top against rotate** (R1 Q6). Watch pass^3 for the
   consistency cost.
7. **`context.ceiling_tokens` 200000 against null** (R1 Q4, R2 Q1 in part).
   Blocked on compaction counts, and limited to adapters that honour the
   ceiling, since Provenant does not compact lanes itself.

Questions that need knobs #1070 lacks (probe turns, advisor, compaction
mode, tool surface, plan first, scope overlap, context handoff) wait for
those knobs. `lanes.max_concurrency` changes wall time only and is not
benched.

## Acceptance

- `mine` re-checks every frozen task in the disposable environment: 3 of 3
  failures on the parent and 3 of 3 passes on the merge.
- A leakage test in a lane environment shows that `git log --all` and
  `git cat-file` cannot reach the merge commit and that GitHub is
  unreachable.
- Provider-free fixture tests on a seeded fixture adapter, like
  `tests/fixtures/current-routing-eval`, reproduce the bootstrap, sign test,
  McNemar and verdicts exactly; a synthetic A/A run yields no `win`.
- `run` refuses held-out without `--release` and refuses a plan whose digest
  changed after the first attempt. `report` rejects a row missing a
  provenance field and stays within 4 KB.
- The pilot has run and its sigma_d and MDE are recorded in
  `docs/evals/replay-bench/README.md`.

## Not doing

- No learned router; log the data first (R1 F3).
- No decision from a single run or single repeat, and no acceptance on the
  two-SE screen.
- No public leaderboard tasks such as SWE-bench Verified: contaminated and
  saturated at the top (R2, R3 F4).
- No LLM judge where a test can decide, and no held-out feedback into
  tuning.
- No bench gate for changes that make no measurable claim; ADR 0014's
  detection-in-use policy still covers them.

## Related

- [Operating model](operating-model.md) R4 (ledger, accepted task) and R9
  (box).
- [ADR 0014](../../adr/0014-comparative-skill-evals-on-suspicion.md):
  conditional held-out evals for skills.
- `skills/evaluate/SKILL.md`: frozen plans, retained attempts and receipts.
