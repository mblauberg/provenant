# Telemetry design

Build contract for the one-pass Rust rewrite under [PORTING.md v7](../PORTING.md).
The [cross-area decisions](README.md#cross-area-decisions) bind state, commands,
records and cut-over. Author route: `codex/gpt-6.1-sol@high` via Fabric.

## Purpose and removals

`provenant-telemetry` reduces run's terminal attempt records with the
bundled price table. It reports supervised usage, API-equivalent cost estimates,
route outcomes, attempt timing, route comparison, cost per accepted delivery and
measurement coverage. Run owns durable records, admission,
the registry and terminal publication. Telemetry owns no store, writer lock,
background collection or provider lifecycle operation; a report failure cannot
signal a provider or change a published outcome.

The council's evidence distinguishes activity from product demand: the four
`stats --json` and five `telemetry sources --json` reads in the earlier workshop
scan were telemetry lanes testing their own code. That host's ledger had zero
usage rows, but the laptop's ledger was about **565 MB**, with **566,185 usage
rows, 46,923 events and 13 deliveries**. This Mac's OTel collector was live,
with about **724 MB** retained and **568 MB of logs from one day**. Those are
collection volumes, not evidence of a product reader. The collector retires
because it has **no reader**, not because it was inactive.

The user has since named a reader: observability that makes it easier to
improve the harness and evaluate configurations, efficiency, performance and
cost-effectiveness. It needs per-route outcomes, time, usage, cost and cost per
accepted delivery, not raw OTel streams, a second store or old history. Every
report reduces run's terminal attempt records and workflow's acceptance facts
on demand; telemetry writes nothing.

| Keep | Contract |
|---|---|
| Supervised usage and route outcomes | Run already records usage, route and outcome; reduce those records on demand. Include all retries once. |
| Exact cost estimates and missingness | Integer arithmetic and reviewed prices; unknown measurements remain unknown. |
| Coverage | State the selected scope and its omissions; never imply whole-account measurement. |
| Attempt timing | Run records the timestamps and provider duration; telemetry derives wall time, admission wait and provider time. |
| Route comparison | `stats --by` groups by route or one route field, with outcome rates, retries, p50/p90 times, usage, cost and missingness. |
| Cost per accepted delivery | Read-only join to workflow's `acceptance` reader at the CLI edge; no telemetry write or delivery store. |

| Drop | Reason and consequence |
|---|---|
| SQLite ledger, writer lock, advisory writes, `record_attempt`, ingest and sources | Duplicate run's durable facts without a product reader. No telemetry supervisor edge or new database. |
| Ledger import and history carry-forward | The new binary never opens, imports or deletes the old ledger. Its history stays on disk for the old install. No import command or receipt. |
| Transcript/receipt discovery and OTel ingest/export/collector | No reader for the raw collection (about 0.5 GB a day); the comparison reader is served by run's records. OTel prompt/tool logging stays off. Unsupervised CLI activity is outside coverage; old transcripts and collector data stay untouched. |
| Delivery telemetry writes and the old accepted-cost report | Workflow retains acceptance evidence. No telemetry `upsert_delivery`, `delivery_report` or delivery rows; the read-only join below replaces them. |
| Changes and retro reports | Still no product reader; `stats --by` is the comparison view. Delete both reducers `scripts/telemetry_changes.py` and `scripts/telemetry_delivery.py`; the retrospect skill keeps using evidence directly. |
| Cross-host summaries, aliases and read-time refresh | Stats is local and read-only. No SSH telemetry operation, top-level `stats`/`retro` aliases or implicit refresh. |
| Weekly quota history and ledger fallback | History is not carried forward. Execution-route/quota owns any future window history, reset interpretation, probes and cooldowns. |
| Model heuristics, parent rollups and compaction telemetry | Exact recorded identity suffices. Run owns context and lineage; parent references never add another usage total. |

## Command and report

The sole telemetry command is:

```text
provenant telemetry stats [--since DATE_OR_RFC3339] [--until DATE_OR_RFC3339] [--by route|adapter|model|account] [--detail] [--json]
```

CLI owns clap, global root/project selection, rendering and the frozen command
contract. Dates mean midnight UTC; timestamps require an offset. Select terminal
times in `[since, until)`, defaulting to all available terminal records; reject
invalid bounds or `since >= until`. `--by` defaults to `route`, the exact
(adapter, model, effort, account label) key; the other values group by that one
field, where adapter is the fact's resolved provider. Selection is host-local
and respects the CLI's project scope. No telemetry MCP tool, import, ingest or sources command remains.

JSON uses CLI's envelope, with `schema_version`, `command`, `status`, `data`
and `error`, capped at **1 MiB for the complete result**. `data` is a
`StatsReport`: selected scope and interval, `by`, price-table identity/review date, usage and cost totals, comparison groups, optional UTC daily
account groups, outcomes and coverage. `--detail` adds daily groups in both
formats. Human output shows at most 12 rows per section (200 with detail),
with omitted counts and `-` for missing values. Sort groups by their exact keys,
missing last. JSON never silently truncates; an oversized report returns the
CLI's bounded error with advice to narrow the interval or scope.

Each comparison group has this shape; counts are `u64`, rates are integer basis
points, durations are milliseconds and money is a six-place decimal string:

```text
{ key: { adapter?, model?, effort?, account? }, attempts, tasks, retries,
  outcomes: { <run outcome>: n }, success_bp, partial_bp, failure_bp,
  wall_ms: { p50, p90, missing }, admission_wait_ms: { p50, p90, missing },
  provider_ms: { sum, missing }, usage: { <category>: { sum, missing } },
  cost: { estimate, known_lower_bound, missing },
  accepted: { deliveries, cost, attempt_wall_ms, cost_per_delivery,
              wall_ms_per_delivery, unaccepted_cost, missing_receipts } }
```

The key holds only the selected fields. Success is `succeeded`, partial is
`input_required`, and failure is every other run outcome; `outcomes` keeps the
exact counts so readers can regroup. Retries are attempts minus distinct tasks.
Percentiles are nearest-rank over the group's known values. A group renders in
roughly 1 KiB, so about 900 groups fit; a larger report takes the bounded error.

Coverage in `data` includes `scope = supervised_terminal_attempts`, selected
attempt count and missing usage/category/price/account/timing/receipt counts.
CLI adds the active count from run's `snapshot`; telemetry does not infer it
from terminal facts. Old-ledger history, unsupervised usage (including
interactive harness sessions) and other hosts are outside coverage. Retention
policy and historical completeness are out of scope; stats filters on `finished_at`. Missing data does not mean zero. A valid empty
reader returns an empty report without creating state. A missing/uninitialised
run store is empty; an existing unreadable, corrupt or incompatible store is
an error, never silently skipped.

Use the binding exits: **0** successful report, including partial measurement
coverage in `data`; **1** failed/refused computation or findings; **2** invalid
arguments, malformed input or unsupported record/config version; **3** cannot
proceed because of setup, I/O or contention; **124** wait deadline; **130**
cancelled. Stats does not wait for attempts and normally cannot return 124.
There is **no exit 4**. CLI owns consistent envelope status/error mapping;
internal diagnostics are bounded, redacted stderr under R8.

## Run input and crate boundary

Run owns `AttemptReader::attempts(filter) -> impl Iterator<Item = Result<AttemptFact>>`,
built from its durable store (`run.sqlite3`, a run implementation detail).
Telemetry reads only through that reader; it never opens the database, crawls
`runs/` or reads referenced output paths. CLI asks the reader to select the
project/window and collects at most **100,001** facts, propagating any read
error before calling `stats`. The 100,001st fact returns `TooLarge`; otherwise
CLI passes the bounded collection's iterator of `AttemptFact` to the reducer.
No partial totals are published. Admission and operation insertion remain
atomic in run's store; fabric-runtime owns identity and federation routing.

`AttemptFact` lives in **records** and implements **`Serialize` only**. Run builds
it from decoded, validated stored records. Its fields are task, attempt number,
operation, project, terminal state, exit, `queued_at`/`started_at`/`finished_at`,
`provider_ms`, resolved route (provider, model, effort, account label), usage
counts (`u64`), output paths, reviewer and `delivery_id`. Usage categories are optional, disjoint
uncached input, cache read, cache write, output excluding reasoning, and
reasoning counts; unknown values stay unknown. `StatsReport` is `Serialize`-only.

Run owns the timing fields ([execution-run](execution-run.md#records-layout-and-publication)).
`queued_at` is when admission accepted the request, `started_at` when admission
granted the ticket, and `provider_ms` the supervisor-observed provider execution
time. `started_at` and `provider_ms` are absent when the attempt never started or
no provider spawned. Telemetry derives wall time as `finished_at - queued_at`,
admission wait as `started_at - queued_at`, and reads provider time as recorded.
A missing or negative interval is counted missing, never zero.

Terminal records are immutable. Every retry contributes usage once; the latest
attempt for a task is the one with the **highest attempt number**. There is no
revision deduplication, event subscription or cursor. Stats reports terminal
attempt outcomes and usage under their recorded routes; active work is reported
separately from run's `snapshot`, not as a terminal task outcome. Dispatch may
carry `delivery_id`, retained in the fact and in run's
`AttemptReader::attempt(AttemptId)` lookup. Workflow owns acceptance evidence;
telemetry exposes no upsert/report API and never opens receipts.

For the accepted columns, CLI collects the distinct `(project, delivery_id)`
pairs in the selected facts (at most 10,000, else `TooLarge`) and calls
workflow's `acceptance(ProjectId, DeliveryId) -> Result<Option<AcceptanceFact>,
ReceiptError>` ([workflow](workflow.md#rust-owner-and-core-model)) for each.
`AcceptanceFact` lives in records, `Serialize` only, and carries the decision
state. CLI passes the resulting `Acceptance` map, keyed by project and
delivery, to the reducer; a missing receipt counts in `missing_receipts`, and a
corrupt one is an error, as for run's store. A group's `accepted.cost` sums its selected attempts that carry an
accepted delivery, including failed retries and reviews; `deliveries` counts
those distinct deliveries, and the per-delivery values divide once at output.
`attempt_wall_ms` sums attempt wall times, so it is attempt time, not elapsed
delivery time. Pending, abandoned and receipt-less spend is `unaccepted_cost`.
Attempts without `delivery_id` stay out of these columns. Only attempts inside
the window count; widen it to cover whole deliveries.

| API | Contract |
|---|---|
| `stats(facts, &Acceptance, &PriceTable, window) -> Stats` | Pure bounded reduction over any iterator of `AttemptFact`; `window` is `StatsWindow { since, until, by, detail }`: it selects `finished_at` in `[since, until)`, `by` picks the group key and `detail` adds daily groups. `Stats` is the result of the reduction: `Result<StatsReport, TelemetryError>`. |
| `validate_prices(&ConfiguredPrices) -> Result<PriceTable, PriceError>` | Telemetry validates its price schema after state's bundled TOML loading. |

Telemetry retains only the allowed `core`, `records`, `config` dependencies.
There is **no run -> telemetry edge**: CLI composes both. The execution-run
PR removes telemetry from run's allowed dependencies in PORTING.md R2, with a
note; telemetry's first build changes its responsibility from ingest to
usage/cost reduction. Core supplies IDs and timestamps; providers normalises
usage; run owns reader consistency, decoding limits and cancellation between
records; CLI owns parsing, cancellation and rendering.

Bound the reduction to 100,000 selected attempts, 10,000 groups and 1 MiB of
retained group-key bytes; enforce limits before insertion. Retain fixed
numeric accumulators, not per-token/event vectors; percentiles retain one `u64`
per attempt per duration, at most 1.6 MB across groups. Return `TooLarge` rather
than partial totals on a bound. Other typed errors are `InvalidFact`,
`InvalidWindow`, `InvalidPrice` and `Overflow`; CLI maps invalid facts/windows/prices to 2 and
overflow/size refusals to 1. Run read errors retain their own version/I/O
classification. No successful totals accompany a failed reduction.

## Prices, account ownership and exact arithmetic

Telemetry owns the price schema (`PriceTable`); state owns how bundled TOML
assets load, including deserialisation and release asset layout.
Replace `config/telemetry-prices.json` with the bundled TOML asset
`config/telemetry-prices.toml`, installed with the release at
`releases/<SourceDigest>/assets/config/telemetry-prices.toml`. State loads the
asset; telemetry validates its schema. Stats writes no price copy or
`telemetry/` subtree. State root and run layout follow the cross-area table:
one host root, top-level `runs/<RunId>/` and run store, not project-nested runs or a telemetry ledger.
There is one user-owned `config.toml`; no telemetry profile or overlay.

The price asset has `schema_version`, `reviewed_at` and `[[models]]` entries
keyed by exact provider/model. Entries carry source references, billing kind
(API, subscription or explicitly free) and optional `u64` rates for uncached input, cache read, cache write and
output excluding reasoning; reasoning uses the output rate in **USD microdollars per million tokens**. Omitted rates mean unknown
(TOML has no null); zero requires an explicitly verified zero rate. Reject
duplicate keys, unknown versions and invalid units. No runtime fetch, model
regex or inferred aliases. Group by the exact resolved provider/model in
`AttemptFact`. Run's reader filter selects production attempts; fixture records
are outside that scope. Unknown identity stays unattributed.

Routing owns **account eligibility**. Run records the resolved, non-secret
account label in each fact's route; telemetry reads that label only. There is
**no `--account` flag**: none of the 1,116 observed dispatches set one. Quota
owns provider-specific window/reset semantics. Providers/execution-route
replaces `scripts/provider_identity.py` with its non-secret scope semantics;
telemetry neither configures accounts nor infers them from email, tokens or
model names.

Providers normalises inclusive wire counts once, subtracting known cached/write
input and reasoning output. Run records the disjoint result; telemetry never
subtracts twice or clamps an inconsistent breakdown. Known token counts sum
with checked `u64` addition; report missing counts alongside each sum.

For every known category, accumulate `t as u128 * r as u128` using **checked
addition into a u128 numerator**. Each u64 product fits u128; a sum may not.
Carry that numerator across categories, attempts and groups, and divide by
**10^6 once at output**, rounding half-up once. Use quotient/remainder for
rounding so adding half a denominator cannot overflow the numerator. Check
rounding and conversion of the final microdollar value to `u64`. Format money
as a six-place decimal string, never `f64`. Reasoning uses the reviewed output
rate once; categories must not overlap. Compute overall totals from unrounded
numerators, not rounded group values.

A complete estimate is null if any included attempt lacks required usage or a
positive-count rate. Report the known-cost lower bound and missing-cost count
separately. Verified-free routes cost exactly zero even with missing usage;
missing categories still count in coverage. Zero tokens need no rate; an unpriced subscription is not free.
Cost is an API-equivalent estimate, not a bill; cost per accepted delivery is
that estimate divided by accepted deliveries, and is null when the cost is.
Weekly quota history is not reconstructed from costs or retained old rows.

## Evaluating configurations

To compare two routes, the evaluate skill freezes its plan, dispatches both arms
through run over the same tasks, then reads `telemetry stats --since START --by
route --json` once for the evaluation's project and window: each arm is a route
group with outcome rates, p50/p90 wall time, usage and cost. Quality comes from
evaluate's graders, not telemetry; quality per hour divides graded passes by the
arm's summed wall time. For real work, the user compares routes over a period by
cost and attempt time per accepted delivery. Coverage excludes unsupervised
interactive harness sessions, and account-level consumption comes from quota
snapshots.

## Collector retirement at cut-over

This is a future **user-authorised checklist for each Mac**, owned jointly by
state's installer and telemetry. It does not authorise any installed changes
in this design lane. Print the complete per-host plan, exact command and every
harness-config key/table removal for the user **before applying**, and obtain
authority covering both Macs. Preserve unrelated user configuration and all
old ledger, transcript and collector data.

1. Before replacing the old install, quiesce old clients without signalling
   healthy providers. From that Mac's retained legacy checkout, plan
   `scripts/install-harness --uninstall-otel-collector`. This uninstalls the
   LaunchAgent, plist and collector binary; it leaves stored data in place.
   Keep the legacy helper available through cut-over even though rewrite
   source retires it.
2. Print the planned removals of `OTEL_*` and
   `CLAUDE_CODE_ENABLE_TELEMETRY` entries from `~/.claude/settings.json`'s
   `env` object, and Codex's `[otel]` table from `~/.codex/config.toml`.
   Print key names and structural edits only, never environment values.
3. After user authorisation, run the printed uninstall command and apply only
   the printed config edits through the installer’s managed-file boundary.
   Coordinate these removals with the installer's other cut-over harness edits.
4. Verify on each Mac that `com.provenant.otlp-collector` is unloaded, its
   managed plist/binary are absent, and the listed telemetry settings are gone.
   Restart clients; check the new binary and a fake-provider stats report.
   Report each host separately; a failure on Mac two remains incomplete work.

No step deletes old data or migrates history. Rollback follows the shared
`pre-rust-rewrite` reinstall contract; re-enabling collection is a separate
user-authorised installed change.

## Tests and build sequence

No direct provider argv, hook, Git or MCP wire parser lives here. Providers
owns recordings of real usage wire forms and its normaliser tests. Telemetry
uses independent typed fixtures after normalisation, never Python/Node oracles,
live provider calls or legacy-ledger fixtures. Use an in-house snapshot helper
where useful; **no insta** dependency.

- Unit tests: zero/unknown/free rates, missing categories, exact resolved route,
  inclusive-count normalisation at the provider boundary, u64 product overflow
  avoided by u128, u128 sum/final-u64 overflow, half-up rounding only at output,
  retries counted once, highest attempt number, active counts from snapshot,
  time/scope bounds, production reader filtering and all aggregation bounds,
  missing timing, nearest-rank percentiles and the acceptance join
  (accepted/pending/abandoned/missing receipt, retries counted once).
  Supply explicit times and data.
- Run integration: immutable terminal facts during publication, bounded reader
  decoding, `finished_at` filtering, active snapshot counts and cancellation.
  No telemetry callback is needed for terminal publication.
- CLI integration at tree landing: real envelope/exits, unsupported versions
  exit 2, missing store creates nothing, corrupt store refuses, partial coverage
  exits 0 with coverage in `data`, 1 MiB cap including envelope, cancellation
  exits 130, deterministic human rows, reader failure before reduction,
  `--by` keys, a corrupt receipt refusing and the 100,001st-fact refusal. Test isolated HOME/state/config only.
  Old-root tripwires prove stats never opens old stores or transcripts.
- Installer integration: isolated fixtures for both Macs' collector/config
  retirement plans, user-authorisation boundary, unrelated configuration
  preservation and old-data byte preservation. Never test against real homes.

1. **Telemetry crate and deletions only.** Build the pure reducer with
   comparison groups and the acceptance join, shared `AttemptFact` and
   `AcceptanceFact` in records and telemetry-owned TOML price schema; replace inert
   telemetry Rust and delete telemetry Python, tests and schemas below. Retain
   telemetry/stats/retro golden cases until CLI step 3 removes the verbs.
   State slice 4 (harness cut-over) deletes the OTel installer helper, Swift
   collector, plist template and collector tests together with installer flags. No clap
   subtree lands here: CLI owns that later. Removing `scripts/telemetry.py` breaks
   `scripts/provider_quota.py:217–220`'s dynamic import and
   `scripts/utility.py:18`'s direct import: the **old quota path goes red**
   until its Rust owner lands. Do not add adapters or preserve dead code to
   keep old tests green (R0/R10).
2. **Run and owner integration.** Run supplies `AttemptReader` and `snapshot`,
   records `queued_at`, `started_at` and `provider_ms`, removes advisory
   telemetry calls and drops its telemetry edge in R2. Workflow supplies
   `acceptance`. Routing
   owns account eligibility; providers replaces identity scopes; quota removes
   ledger history/fallback; workflow removes delivery upsert/report expectations.
   Each owner deletes its surviving shared callers.
3. **CLI and install integration.** CLI lands the sole stats command with
   `--by`, composes run's reader and workflow's `acceptance`, and removes
   aliases/help; state coordinates the printed two-Mac retirement
   checklist at authorised cut-over. Heavy checks run in GitHub Actions;
   locally run only fmt/clippy and targeted tests for touched crates.

## Deletion inventory and budget

Build PRs delete subjects and their tests together; the following is an owner
inventory, not authority to edit old stores, installed files or whole shared
modules. Count each deletion once, including workflow's reducer hand-off.

| Owner | Delete or replace |
|---|---|
| Telemetry | `scripts/telemetry{,_ingest,_costs,_report,_changes,_delivery,_hosts}.py`, `scripts/provenant-telemetry`, `tests/test_telemetry.py`; all inert Rust under `tools/front-door/crates/telemetry/src/`, including `pending`, Unicode generator/tables and differential harness. |
| Telemetry data | Four `schemas/telemetry-{usage,event,quota-series,quota-event}-row.schema.json` files and JSON price asset with its TOML replacement. Repair `schemas/README.md`; shared record-schema tests retire with their subjects. |
| State slice 4 (harness cut-over) | `scripts/install-otel-collector`, Swift collector and plist template under `tools/otlp-collector/`, `tests/test_otlp_collector.py`; state removes OTel flags/wiring from `scripts/install-harness` and counts the OTel slices of `tests/test_install_harness.py`. Retained legacy install supplies the cut-over uninstall helper. |
| Providers/execution-route | Replace `scripts/provider_identity.py`; update `scripts/quota_display.py:9`'s window-label configuration and telemetry renderer import, remove account help/`--account` from `skills/orchestrate/scripts/dispatch_run.py` and `cf_dispatch.py`, and identity/account tests including `runtime/fabric/tests/surface.test.ts:253`. Routing owns eligibility; run records the resolved label. |
| Quota | Remove `_telemetry`, `state_root`, `ledger_windows`, `record_series` and telemetry writes in `scripts/provider_quota.py`; remove ledger fallback from `scripts/quota_providers/codex.py` and removed-subject quota tests/oracles. |
| Run/utility/Fabric | Remove dispatch telemetry assertions and callbacks; telemetry import/`record_call` in `scripts/utility.py`; `telemetry_summary` and registration in `scripts/fabric_hosts.py`. Do not remove unrelated identity/quota/peer behaviour. |
| CLI step 3 | Delete telemetry/stats/retro golden cases with their verbs and help. Replace telemetry command routing; remove root stats/retro aliases; replace/remove `tools/front-door/src/help/{telemetry,stats,retro}.txt`, stale telemetry fixture setup in `tools/front-door/tests/common/mod.rs:53`, and retired-command tests. |
| Docs/skills | Update telemetry/host/routing runbooks, README and architecture references, delivery/retrospect promises and code-review's `multi-agent-review.md` plus orchestrate's `routing-and-tiers.md`, `worker-liveness.md` and `herdr-panes.md` references. Remove ledger, sources, ingest and accepted-cost instructions; preserve unrelated policy. |

State alone deletes/counts `tests/test_host_identity.py` and the OTel slices
of `tests/test_install_harness.py`; both are excluded from telemetry's budget.
Shared `roots.py`, `host_identity.py` and `harness_profile.py` helpers are
deleted with their **last importer**, following execution-run's rule: steps 1–2
remove telemetry/run/provider imports; step 3 removes remaining CLI/installer
imports, and that last-importer owner deletes and counts each helper. This lane
does not claim supervisor's `attempt_storage.py` or state's whole-file deletion
of `tests/test_install_skills.py` with the old installer.

The telemetry-only Python files listed above total **6,231 lines**, including
`tests/test_otlp_collector.py`, excluding the shell wrapper and two state-owned
test overlaps. Inert telemetry Rust totals **5,810 lines**;
there are zero telemetry TypeScript files. Target **at most 1,500 new Rust
lines including tests** for the reducer and its command tests: forecast
Python/TS/Rust net at most **−10,541 lines** (1,500 − 6,231 − 5,810), excluding
shared-owner cleanup and data/shell/Swift deletions. This is a budget, not
measured savings; each build PR reports actual `Net lines:` and justifies growth.
