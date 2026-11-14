# Telemetry and quota
Owner: provenant-telemetry
Anchor: [`telemetry tests`](../../tools/front-door/crates/telemetry/src/tests.rs)

Reports are local reads over retained attempt facts, priced from
`config/telemetry-prices.toml`. Flags and output fields are in
`provenant telemetry --help` and the [telemetry spec](../specs/telemetry.md);
this page holds the steps and sharp edges. Cost is an API-equivalent estimate,
not a bill, and an unknown cost is never zero: unpriced attempts make a total a
lower bound.

## The usage ledger

Settled attempts are recorded in `<state>/usage-ledger/`, which outlives the run
store, so it keeps lane usage after run history is removed. Each format-2 ledger
row requires graph and node identities with its priced settlement. Unreadable
ledger lines are reported by segment and line and kept, not repaired. The
format-1 reset must discard `usage-ledger/`, the prior schema-1 `ingest/`
segments and `ingest-cursors.json`. Format 2 reads only the format-2 ledger and
schema-2 ingest rows. Removing the old cursors lets later requests replay their
selected transcript contents rather than skipping them by a matching cursor
digest; each request's `--since` bound still applies. The reset may also remove
the inert single-file `ingest.jsonl` and `ingest.jsonl.unmigrated` paths.

## Native session ingest

`provenant telemetry ingest [--provider claude] [--since T]` records native
Claude usage from every registered account directory and every other
`~/.claude*` directory under home. Registered accounts use their label; any
other directory, the default `~/.claude` included, is `unattributed`. Rows are
keyed by session and message, so reruns add nothing. The chair's SessionStart
hook ingests the last day automatically within a three-second budget. The store
(the `ingest` directory in the state root, one `YYYY-MM-DD.jsonl` segment per
UTC day, with no whole-size limit) holds counts, model, account, timestamps and
IDs only, never prompt, reply, tool or path text. A message that straddles
UTC midnight stays under the day it was first stored. Format 2 reads the
segmented schema-2 store only; it does not import a prior single-file
`ingest.jsonl` store.

To attribute chair sessions, launch with the registered account's directory:
`CLAUDE_CONFIG_DIR=~/.claude-work claude`. `provenant telemetry cost --by account`
reports usage per account.

## Reading the reports

- `telemetry stats [--since T]` is the default report, and `--json` carries the
  same summary rows. A result over 1 MiB is refused with `RESULT_TOO_LARGE`, so
  narrow it with `--since`. `telemetry stats --help` lists its extensions. `T` is
  seconds ago, a date or RFC3339.
- `telemetry cost [TARGET] [--by route|account|node|role|preset|pr|band|day]` is
  small enough to call from inside a lane. `--by pr` joins the local contribution
  store to PRs GitHub reports as merged (needs `gh`); missing weights count at
  full value and are reported as unknown allocations.
  `telemetry value` gives one row per account with a
  quota reading and a configured plan; compare a full billing period before
  acting on it.
- `telemetry mileage` is what one quota point costs: it joins quota history with
  ingested usage, so run ingest first. Do not pass `--model`: quota deltas cover
  every model, so a model filter understates mileage. `step_change` is `flagged`
  only for a fall of at least 20% with ten priced segments on each side.
- Cache-read share (`cache` in `--json`) is the figure to raise: compact at task
  boundaries while the cache is warm. No cache split means not reported, not a miss.
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
waits for the observation window (`--outcome-days`, 14) and 20 mature merges per
arm. Suggestions are advisory (`accept: null`): nothing writes an override, and
`route override set` is a pacing override, never a model-choice command.

## Price sheet

`provenant telemetry prices refresh [--source openrouter|litellm]` writes a
proposal (`telemetry-prices.proposed.json`) and never edits the sheet: exit 0
means nothing to review, exit 5 means a proposal awaits review. Only models with
`openrouter_id` or `litellm_id` are compared; a difference between sources above
`discrepancy_threshold_bps` is listed, never resolved. Vendor pages remain the
record for subscription-route equivalents. `max_review_age_days` makes `stats`
warn when `reviewed_at` is stale.

## Usage counters

`telemetry stats --usage` counts CLI verbs and Fabric MCP tools from
`usage.jsonl` (5 MB rotation, two files kept, so a 30-day query on a busy host
may cover less); arguments, prompts and results are never logged.
`usage.project_attribution = true` adds the project hash, which reveals which
projects use Provenant to anyone who can read the local log, so it is off by
default. Per-project rows are subsets of the aggregate: do not sum them.

## Quota

`provenant quota` reads cached observations; `quota tick` refreshes due
accounts (`--trigger`, `--power` record why); `quota display` sets the menu bar
choices. Quota is admission advice: a reset or bookkeeping failure never grants
permission to stop a healthy provider.

Each refresh that differs from the last reading appends a record to
`quota/history/<account_key>.<adapter>.jsonl` (rotating at half
`domains.quota.history_max_bytes`); nothing is backfilled and no provider call
is added, so resolution is the refresh cadence. `provenant quota history
[--account LABEL] [--since T] [--limit N] [--json]` lists readings.

Record plan prices per account, one entry per price change so a rise or switch
keeps the old figures. Nothing converts currencies; a free plan has `price = 0`
and may carry `notional_usd`:

```toml
[[domains.accounts.entries.claude-work.plans]]
name = "max_5x"
price = 100.0
currency = "USD"
effective_from = "2026-01-01"
```

Keys are in `provenant config schema`.
