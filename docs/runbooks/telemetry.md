# Telemetry and quota
Owner: provenant-telemetry
Anchor: tools/front-door/crates/telemetry/src

`provenant telemetry stats` reduces retained terminal attempt facts with prices
from `config/telemetry-prices.toml`. Optional `--since`, `--until DATE`,
`--run-id ID` and `--detail` bound or extend the report. `--since` takes a
number of seconds ago, a date or an RFC3339 timestamp. Use `--json` for
structured output. The contract is in the [telemetry spec](../specs/telemetry.md).

Cost is an API-equivalent estimate, not a bill. The report separately counts
attempts whose price-table entry is marked as a subscription, including those
whose token usage or API-equivalent price is unavailable. This classification
does not verify account billing. An unknown cost is not zero.

To inspect native Claude Code sub-agents for a session, run
`provenant telemetry stats --claude-subagents PATH`, where `PATH` is that
session's `subagents` directory under `~/.claude*/projects/<project>/<session>`.
The directory must contain only regular, non-symlink `agent-*.jsonl` files. This is an
explicit import and produces a separate usage and API-equivalent cost report
grouped by model; it does not add those calls to supervised attempt totals.
The import uses usage metadata, not message bodies. It cannot be combined with
`--run-id`, `--since`, `--until` or `--usage`.
With `--json`, the `native_claude_subagents` section reports agent and message
counts, usage, price-table billing and cost by model, with `cost_basis` set to
`api_equivalent_estimate`. A model's `cost_usd` is
null when its usage or price is incomplete. Model prices are matched exactly;
dated aliases are not inferred.

## Offline timeline

`provenant telemetry trace [--out FILE] [--since 604800] [--run ID] [--peers]`
writes retained attempts as Chrome Trace Event JSON (default
`provenant-trace.json`), with one process per host and one thread per attempt;
arrows join a child attempt to its parent, across hosts too. Open the file in
ui.perfetto.dev or chrome://tracing; no collector is needed. Add `--peers` to
read each configured peer's newest 100 lanes as well, whoever placed them; an
unreachable peer is reported and skipped.

## Local usage counters

`provenant telemetry stats --usage --since 2592000` counts CLI verbs and Fabric
MCP tools over the last 30 days, including installed units with zero calls.
Omit `--since` for all retained calls. Counts cover this host's `usage.jsonl`
and `usage.jsonl.1` under the selected Provenant state directory. A fresh host
counts nothing until the state root exists. The report includes its own call
on the next invocation.

Each CLI invocation records `cli.verb`, its canonical verb path, exit code,
duration in milliseconds and UTC timestamp. Parse failures use `parse`, help
uses `help`, and version uses `version`. Each MCP tool call records `mcp.tool`,
the installed tool name, outcome and timing; unrecognised names use `unknown`.
Arguments, prompts and results are never logged. Skill loads and scripts are
not counted. These counters are separate from token and cost reports, and do
not enable OTel prompt or tool logging.

Set `usage.project_attribution` to `true` to include the existing `ProjectId`
hash, rather than a path or name, in usage events (as `trace.project`) and group counts by project.
This reveals which projects use Provenant to anyone who can read the local
usage log, so it is off by default. Per-project rows are subsets of the
aggregate row for each kind and unit; do not sum them with the aggregate.
Text output labels these rows with `project=<id>`. JSON includes `project_id`
and `scope: "project"` on these rows; aggregate rows omit both fields.

Writes are best effort: one small `O_APPEND` write, no sync or background
collector. I/O failures leave command results unchanged. Only rotation takes
an exclusive nonblocking lock; a busy rotation lock still allows appends.
MCP rotation runs on existing workers, keeping it off the event loop. At
5 MB the current log replaces `.1`. Concurrent writes can briefly exceed the
cap. Reports read without locks, tolerate a missing `.1`, and skip malformed
or incomplete lines. A concurrent rotation can make a report approximate.
Only two logs are retained, so a 30-day query can cover less than 30 days on a
busy host. No config, provider or run-store state is created by these
counters.

## Quota

`provenant quota` reads cached observations. `provenant quota tick`
refreshes due accounts; use `--trigger` and `--power` to record why the
refresh ran and the current power source. `provenant quota display [--headline
auto|5h|weekly] [--icon most_constrained|ACCOUNT]` sets the menu bar choices,
then shows quota. `provenant telemetry stats --usage` reports local invocation
counts. Quota is admission advice; a reset or bookkeeping failure never grants
permission to stop a healthy provider.

### Quota history

The snapshot file for each account is overwritten on every refresh. To keep
the series, each refresh also appends one record to
`quota/history/<account_key>.<adapter>.jsonl` when the reading differs from the
last one. An identical reading appends nothing. A record holds the observed
time, account key and label, provider, the plan the account reports, every
window (pool, window seconds, used basis points, reset time), credit balance
and reset credits. It marks a `reset` when a window's use falls or its reset
time advances, and a `plan.change` when the plan label changes between two
known values. `tier` is null until a provider response carries one. Records
hold no credentials, provider payloads or prompt text. Nothing is backfilled,
and no provider call is added: readings are what the existing refresh already
fetches, so resolution is the refresh cadence.

Appends run under `quota.lock`, so lines never interleave. The file rotates to
`.jsonl.1` at half `domains.quota.history_max_bytes` (4 MiB by default, so the
pair stays under the cap) and the newest record always lands in the current
file. Readers skip torn lines.

`provenant quota history [--account LABEL] [--since T] [--until T] [--limit N]
[--json]` lists readings, oldest first, newest `--limit` kept. `--account`
takes a label or key. Times are seconds ago, a date or an RFC3339 timestamp.
`--json` returns `schema_version`, `count`, `matched`, `truncated` and
`readings`; each reading adds `plan_price`, the account's price in force at
that reading, or null.

Set prices in `config.toml` as an array of tables per account, one entry per
price change, so a rise or a plan switch keeps the old figures:

```toml
[[domains.accounts.entries.claude-work.plans]]
name = "max_5x"
price = 100.0
currency = "USD"
effective_from = "2026-01-01"
```

Prices are USD unless `currency` says otherwise (`price_usd` is accepted as
the name of `price`), and nothing converts currencies. A free plan has
`price = 0` and may carry `notional_usd`, the list price it replaces; history
shows both. An account may also list extra `directories` signed into it; they
are schema only until native ingest lands. The keys are listed in
[config keys](../reference/config-keys.md). No value or mileage report reads
this yet.

## Routing

`provenant route list` shows the catalogue and pools. Select a route or exact
model on dispatch and retain the resolved route from the attempt record.
