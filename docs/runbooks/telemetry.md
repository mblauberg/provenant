# Telemetry and quota
Owner: provenant-telemetry
Anchor: [`telemetry tests`](../../tools/front-door/crates/telemetry/src/tests.rs)

Reports are local reads over retained attempt facts, priced from
`config/telemetry-prices.toml`. Flags and output fields are in
`provenant telemetry --help`; the invariants (honest totals, the usage ledger,
exact money) are in the [telemetry spec](../specs/telemetry.md). This page holds
the steps and sharp edges.

## Native session ingest

Run `provenant telemetry ingest` to record native session usage. To attribute a
chair session to an account, see [account switching](configuration.md#chair-account-switching).

## Reading the reports

- `telemetry stats [--since T]` is the default report, and `--json` carries the
  same summary rows. If a result is too large, narrow the time range.
- `telemetry cost [TARGET] [--by route|account|model|graph|node]` is small enough
  to call from inside a lane. The `--by` list is declared once in `CostBy` and
  checked against this line by a test; a new grouping must change both.
- `telemetry value [--account LABEL] [--window POOL] [--since T]` gives one row
  per account with a quota reading and a configured plan. Each requested window
  carries `cost_per_full_window` (API-equivalent cost scaled from the observed
  usage), `value_multiple` against the plan price in force, `unused_share_at_reset`
  with its timestamp and sample count, and `trend` in full-window API-equivalent
  cost across completed windows. The trend uses costs through the last pre-reset
  reading, reports its change in microdollars and is unavailable if a cycle
  lacks bounds or fully priced usage. Every
  figure states its coverage; an unpriced bucket is a named lower bound, never
  zero. Credit accounts add `credit_runway` (draw-down per day and days to
  exhaustion), or a named unavailable reason when the readings are missing.
  `price_source` and `window_source` name where the defaults came from. The
  value multiple uses a USD plan price or explicit `notional_usd`; unsupported
  currencies are named unavailable. `--since` limits the summary, not a bounded
  quota cycle's cost through its quota observation; a window without valid cycle
  bounds or an observation within them has unavailable full-window cost.
  Compare a full billing period before acting on any of it.
- `telemetry mileage` is what one quota point costs: it joins quota history with
  ingested usage, so run ingest first. Do not pass `--model`: quota deltas cover
  every model, so a model filter understates mileage.
- Cache-read share (`cache` in `--json`) is the figure to raise: compact at task
  boundaries while the cache is warm.
- `telemetry trace [--out FILE] [--peers]` writes Chrome Trace JSON for
  ui.perfetto.dev.

## Offline pipeline outcomes

Export the main-branch CI history and pass it to the local report, which itself
uses no network:

```sh
gh run list --branch main --limit 1000 --json headSha,conclusion,createdAt,workflowName > main-ci.json
provenant telemetry stats --suggest --ci-file main-ci.json --main-ref main --json
```

Check `pipeline.outcomes.ci_coverage_from` against the merges compared: older
merges have null CI counts. Without `--ci-file`, CI is `not_measured`. Ranking
waits for enough mature observations. Suggestions are advisory: nothing writes an
override; `route override set` records one by hand.

## Price sheet

`provenant telemetry prices refresh [--source openrouter|litellm]` writes a
proposal and never edits the sheet; review it before updating the sheet. Only
models with `openrouter_id` or `litellm_id` are compared; a difference between
sources above `discrepancy_threshold_bps` is listed, never resolved. Vendor
pages remain the record for subscription-route equivalents.
`max_review_age_days` makes `stats` warn when `reviewed_at` is stale.

## Usage counters

`telemetry stats --usage` counts CLI verbs from the local usage log, which rotates
and may cover less than the requested period. `usage.project_attribution` adds
the project hash, which reveals which projects use Provenant to anyone who can
read the local log, so it is off by default. Per-project rows are subsets of the
aggregate: do not sum them.

## Quota

`provenant quota` reads cached observations; `quota tick` refreshes due accounts
(`--trigger` and `--power` record why); `quota display` sets the menu bar choices.
Quota is admission advice: a reset or bookkeeping failure never grants
permission to stop a healthy provider.

Each refresh that differs from the last reading appends a record to
`quota/history/<account_key>.<adapter>.jsonl` (each file rotates at half
`domains.quota.history_max_bytes`); nothing is backfilled and no provider call is
added, so resolution is the refresh cadence. `provenant quota history [--account
LABEL] [--since T] [--limit N]` lists readings.

Record plan prices per account in `domains.accounts.entries.{id}.plans`, with an
entry for each price change. Nothing converts currencies. A foreign-currency
plan needs `notional_usd` for `telemetry value` to state a value multiple. A
free plan carries `notional_usd` so the multiple uses its notional, not a claim
that the plan was paid for.
