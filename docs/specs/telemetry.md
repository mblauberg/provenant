# Telemetry

## Purpose and ownership

`provenant-telemetry` reduces run's terminal attempt records with the bundled
price table. It reports supervised usage, API-equivalent cost estimates, route
outcomes, route comparison and measurement coverage. It owns no store, writer lock, background collection or
provider operation, so a report failure cannot signal a provider or change a
published outcome.

The reader is the user improving the harness and evaluating configurations,
efficiency, performance and cost-effectiveness. That needs per-route outcomes,
usage and cost, not raw OTel streams or a second store. Every report reduces
run's terminal attempt records on demand. The command edge also keeps a bounded
local demand log; the reducer stays pure.

The [cross-cutting decisions](README.md#cross-cutting-decisions) bind state
root, command envelope and exit codes.

## Boundaries

| Area | Owns | Delegates |
|---|---|---|
| Telemetry | Reducer, report shape, price schema, trace export, demand counter record kinds | Records, admission, terminal publication (run) |
| [Run](run.md) | Durable attempt facts and the `AttemptReader` | Nothing to telemetry; there is no run to telemetry dependency |
| [CLI](cli.md) | clap grammar, root and project selection, rendering, composing run, workflow and telemetry | Reduction |
| [State](state.md) | Loading the bundled price asset | Price schema validation |
| [Routing](routing.md) | Account eligibility, quota windows, reset semantics, probes | Reading the resolved label from run's fact |

Telemetry depends only on `core`, `records` and `config`. CLI composes the
crates. Core supplies IDs and timestamps; providers normalise usage; run owns
reader consistency and decoding limits.

## Contract

### Commands

```text
provenant telemetry stats [--usage] [--since D] [--until D] [--run-id ID] [--detail] [--json]
provenant telemetry trace [--out FILE] [--since D] [--run ID] [--json]
provenant quota show | display [--headline auto|5h|weekly] [--icon ID] | tick
```

`provenant help telemetry` and `provenant help quota` own the flags. Dates mean
midnight UTC and timestamps require an offset; `--since` also takes a day count
such as `7d`. Terminal times are selected on `finished_at` in `[since, until)`.
Invalid bounds or `since >= until` are refused. Selection is host-local and
respects the project scope. There is no telemetry MCP tool and no `--account`
flag.

### Report

The JSON result uses the CLI envelope and its 1 MiB cap. `data` holds
`scope`, the `window`, the price table identity and review date (`prices`),
`totals`, `routes`, `latest_task_outcomes` and `coverage`. `--detail` adds
`daily_accounts`, the same totals grouped by UTC day and route. Human output is
one line: the attempt count and the cost, or "at least" the known lower bound
when any cost is unknown. JSON never truncates silently: an oversized report
returns the CLI's bounded error with advice to narrow the interval.

Routes key on the exact resolved (provider, model, effort, account label),
with missing values last. Every totals block (overall, per route and per day)
carries the attempt count, per-category token usage as known and missing
counts, cost, and exact outcome counts. `latest_task_outcomes` counts the
highest-numbered selected terminal attempt per task. Conventions:

- Outcome counts are exact, one per terminal state, so readers can derive
  success and failure rates.
- Sums add known values only; `missing` counts the rest. Missing data never
  means zero.
- Money is a six-place decimal string with the integer microdollar value.
  `cost.complete` is null when any attempt's cost is unknown;
  `known_lower_bound` and `missing_attempts` are always present.

Coverage states `scope = supervised_terminal_attempts`, the selected attempt
count and the counts missing usage, per-category usage, price and account.
Unsupervised interactive
sessions, other hosts and account-level consumption are outside coverage;
quota snapshots carry the latter.

An absent or uninitialised run store gives an empty report and creates no
state. An existing unreadable, corrupt or incompatible store is an error.

### Run input

Telemetry reads only through run's `AttemptReader`; it never opens the
database, crawls `runs/` or reads output paths. Reduction is bounded to
100,000 attempts, 10,000 groups and 1 MiB of group-key bytes, checked before
insertion; a bound returns `TooLarge`, never partial totals. Accumulators are
fixed numeric values.

### Trace export

`telemetry trace` writes retained attempts as Chrome Trace Event JSON (default
`provenant-trace.json`) for ui.perfetto.dev or chrome://tracing. It uses one
process per task and one thread per attempt, with microsecond `ts` and `dur`,
the outcome as the slice name and a `queued` instant event. `--run` is applied
by the store because attempt facts carry no run ID. `--since` filters on
`finished_at`. The export reads the same bounded reader and needs no
collector.

### Prices and arithmetic

Telemetry owns the price schema; state loads the asset
`config/telemetry-prices.toml`, installed under
`releases/<SourceDigest>/assets/config/`. It has `schema_version`,
`reviewed_at` and `[[models]]` entries keyed by exact provider and model, each
with source references, a billing kind (API, subscription or explicitly free)
and optional `u64` rates in USD microdollars per million tokens for uncached
input, cache read, cache write and output. Reasoning uses the output rate once.
An omitted rate is unknown; zero needs an explicitly verified zero rate.
Duplicate keys, unknown versions and invalid units are refused. There is no
runtime fetch, model regex or inferred alias, and stats writes no price copy.

Providers normalise inclusive wire counts once; telemetry never subtracts
again or clamps an inconsistent breakdown. Token sums use checked `u64`
addition. Cost accumulates `tokens * rate` in a checked `u128` numerator across
categories, attempts and groups, divides by 10^6 once at output and rounds
half-up once. Totals come from unrounded numerators. Floating point is never
used.

A complete estimate is null if any included attempt lacks required usage or a
positive-count rate; the known-cost lower bound and the missing-cost count are
reported separately. Verified-free routes cost exactly zero even with missing
usage. Zero tokens need no rate. An unpriced subscription is not free. Cost is
an API-equivalent estimate, not a bill.

### Local demand counters

The binary edge writes a best-effort `usage.jsonl` under the state root, shared
by CLI completion and the Fabric MCP transport. Records have kind `cli.verb`
or `mcp.tool`, the canonical verb or installed tool name (`unknown` for
unrecognised tools), a timestamp, a duration in milliseconds, and the CLI exit
code or MCP outcome. Argument, prompt and result text never enter the log, and
skill loads and scripts are not counted.

Each line is at most 512 bytes and is one `O_APPEND` write with no lock, tail
read or sync. Only rotation takes an exclusive nonblocking lock, and a busy
lock still permits appends. CLI completion and existing MCP workers rotate at
about 5,000,000 bytes into the single `usage.jsonl.1`; the MCP event loop never
rotates or locks. Concurrent writes can briefly exceed the cap. I/O errors
discard the event without changing the call.

`telemetry stats --usage` reads both files without locks, skips torn lines and
tolerates a missing backup, then sorts units by call count. The CLI grammar and
MCP catalogue supply zero-call rows. Retention is bounded by size, not days, so
counts are observed demand, not proof of inactivity beyond the retained logs.
`PROVENANT_USAGE_STATE_DIR` isolates the counters from other state.

### Quota display

`quota show` reads cached observations, `quota tick` refreshes due accounts
and records a pacing tick, and `quota display` stores the menu bar choices
(`--headline`, `--icon` taking `most_constrained` or an account entry ID or
unique label) and then shows quota. With no option nothing changes. Probe
plans, windows, reset semantics, cooldowns and Claude identity attribution
belong to [routing](routing.md). An authentication failure retains the last
snapshot and marks its row stale; the menu bar projects retained deadlines
against the current time and never reads staleness as zero usage.

If the stored `display.icon` account ID is no longer registered, `quota show`
returns `display.icon = "most_constrained"` and a `display_icon_warning` field
in the JSON report. Human output includes one warning line. Resolution uses
all registrations, regardless of the adapter filter, and does not change config.
Reports omit `display_icon_warning` when the icon choice is valid.

## External contracts

- Chrome Trace Event format for the trace file, opened in a third-party viewer.
- Provider usage counts as normalised by providers; telemetry trusts the
  disjoint categories run records.
- OTel prompt and tool logging stays off; telemetry reads no OTel data.

## Failure and recovery semantics

Exit codes follow the CLI contract. A successful report exits 0, including
partial coverage in `data`. Invalid arguments, malformed input and unsupported
record versions exit 2. Setup, I/O or contention exits 3. Typed reducer errors
are `TooLarge`, `InvalidFact`, `InvalidWindow`, `InvalidPrice` and `Overflow`;
`stats` maps every one of them to an invalid result (exit 2). An unreadable or
invalid bundled price file is a setup error (exit 3). Run read errors keep
their own version and I/O classification. No
successful totals accompany a failed reduction. Stats does not wait for
attempts. Diagnostics are bounded, redacted stderr.

Counters are lossy by design: a failed write, torn line or racing rotation
makes a count approximate and never fails the command.

## Deliberate cuts

| Cut | Reason |
|---|---|
| SQLite ledger, writer lock, `record_attempt`, ingest and sources commands | Duplicated run's durable facts and had no product reader. |
| Importing or deleting the previous ledger | The binary never opens it; a second history invites double counting. |
| Transcript and receipt discovery, OTel ingest, export and collector | No reader for raw collection (about 0.5 GB a day); run's records serve the comparison. |
| Delivery telemetry writes and a delivery report | Workflow keeps acceptance evidence; telemetry does not read it. |
| Accepted and unaccepted cost, cost per accepted delivery, `missing_receipts` | Needs a join from attempts to workflow's acceptance reader that was never built; the report has no delivery columns. |
| Wall, admission-wait and provider timing with p50 and p90 percentiles | Attempt facts feed no timing columns into the reducer; the trace export carries attempt timing instead. |
| Retry and task counts, success, partial and failure rates in basis points | Outcome counts and `latest_task_outcomes` let a reader derive them. |
| Changes and retro reports | No product reader; the route comparison is the product view. |
| Cross-host summaries, top-level `stats` and `retro` aliases, read-time refresh | Stats is local and read-only. |
| Weekly quota history and ledger fallback | Routing owns any window history; costs never reconstruct it. |
| Model heuristics, parent rollups, compaction telemetry | Exact recorded identity suffices; parent references never add a second usage total. |
| An account selector in telemetry | The account label comes from the resolved route and `dispatch --account` pins it at dispatch, so reports need no selector. |
| Telemetry MCP tool | Reports are an operator command, not an agent surface. |

## Verification

- `crates/telemetry/src/tests.rs` covers grouping, the attempt, group and key bounds, overflow and exact rounding, and
  missingness against unknown prices and usage.
- `tools/front-door/tests/usage.rs` covers the demand log: record kinds,
  rotation, torn lines and isolation through `PROVENANT_USAGE_STATE_DIR`.
- `tools/front-door/tests/quota.rs` covers `quota show`, `display` and `tick`.
- Evaluations of two configurations run both arms through run, then read
  `telemetry stats --since START --json` once for the project and window.
  Quality comes from the evaluate skill's graders, not telemetry, and the
  report carries no timing, so elapsed time comes from the trace export.
