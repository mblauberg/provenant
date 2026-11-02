# Token-per-merged-PR study (#2315)

Question: after the cut-over, do merged PRs cost fewer tokens than before?
This is a deterministic offline reduction of telemetry that already exists. It
uses no LLM, no network, no new counters and no synthetic runs.

## Method

Implementation: `provenant_telemetry::study::reduce` (library) and
`cargo run -p provenant-telemetry --example study -- INPUT.json [--json]`.
No `provenant telemetry study` subcommand exists yet: wiring it needs edits to
the CLI's module list and dispatcher, which were outside the slice that added
the reducer. The example binary is the stopgap.

Input (`StudyInput`, JSON, all times unix seconds):

- `cut_over`: the commit and date that separate the cohorts (stated in output).
- `baseline` / `enabled`: `[from, until)` merge-time intervals. They must not
  overlap. Each cohort is the merged PRs whose merge time falls in its interval.
- `prs`: merged PRs from first-parent history (`number`, `merged`, `lines`,
  `work_items`).
- `attempts`: one row per terminal attempt (`AttemptRow::from_fact` projects an
  `AttemptFact`), with retained `work_item`, `role`, route and summed tokens.
- `chair`: one row per chair session with the PR it was attributed to and its
  transcript tokens (`null` = unread).

Rules:

- Work is attributed to the cohort of its PR's **merge time**, not its own
  finish time. An attempt or chair session that finished before the cut-over
  for a PR merged after it counts once, in the enabled cohort. Work joined to
  no cohort PR (unlinked or abandoned) has no merge time and falls back to its
  own finish time; work joined to a PR merged outside both cohorts is counted
  in neither.
- Duplicate PR numbers, attempt IDs and chair `session` values are rejected
  (the reducer returns an error); none are silently deduplicated.
- An attempt joins a PR when its `work_item` is `#N`, `pr:N` or one of the
  PR's `work_items`. A label claimed by two PRs joins nothing.
- Tokens are the sum of the disjoint usage categories. Input and output must
  both be measured; cache and reasoning counters add when present, since a
  provider without caching or reasoning omits them. An attempt missing input
  or output adds zero tokens and is counted in `linked_without_tokens`; it is
  never silently treated as measured.
- Each token lands in exactly one category: `chair`, `failed_attempt`
  (non-success terminal state), `review` (role contains "review"), `repair`
  (role contains "repair" or "fix"), else `worker`. Per-category totals sum to
  the cohort total.
- Attempts and chair sessions that join no cohort PR are shown beside the
  totals, never inside them: `abandoned` (a work item that no merged PR
  claims), `unlinked` (no or ambiguous label; chair sessions with no PR).
- The plain-text report prints each cohort's `[from, until)` interval,
  attribution coverage (linked/window attempts, attempts without tokens) and
  chair coverage (sessions, linked, unread, share of PRs with a read counter).
- Reported per cohort: merged PRs, aggregate, mean and median tokens per PR
  (median of an even count is the floor of the middle pair's mean), size
  buckets by changed lines (small ≤ 50, medium ≤ 300, large, unknown), token
  share by route, attribution coverage and chair coverage.

Verdict:

| Verdict | Condition |
| --- | --- |
| `pending` | either cohort has fewer than 10 merged PRs |
| `inconclusive` | in either cohort: any attempt without a token counter (linked or beside the totals), any chair session with an unread counter, or no chair counters attributed; or attribution or chair coverage differs by more than 10 percentage points between cohorts. The reasons name which counters are missing |
| `comparable` | otherwise; the percentage changes in mean and median are then filled in |

A `comparable` result is a descriptive comparison, not a causal claim: the
cohorts differ in time, PR mix and model pricing. Read the size buckets and
route mix before drawing a conclusion.

## Controlled replay (deferred)

A controlled comparison would replay the same recorded tasks under both
configurations. Without live runs it can only be specified, not measured: a
deterministic offline comparison over recorded fixtures would reduce paired
`StudyInput` fixtures with identical PR sets and report per-task deltas. That
needs a recorded fixture corpus that does not exist yet, and synthetic runs are
excluded from this study, so it is deferred until such a corpus is captured.

## Results template

Copy into the issue when real data is available.

```text
Cut-over: <commit> on <date>
Baseline cohort: merged <from> to <until>, N = <n>
Enabled cohort:  merged <from> to <until>, N = <n>
Verdict: pending | inconclusive | comparable   Reasons: <...>

                      baseline   enabled   change
Aggregate tokens/PR
Mean tokens/PR
Median tokens/PR
Size: small / medium / large / unknown (median, n)
Category: chair / worker / review / repair / failed
Route mix (token share)
Attribution coverage (linked attempts)
Chair coverage (PRs with a read chair counter; unread sessions)
Beside totals: abandoned / unlinked attempts, unlinked chair sessions
Data sources and extraction dates:
```
