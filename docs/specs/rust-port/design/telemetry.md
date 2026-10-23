# Telemetry design

Proposal for the workshop chair and build lane, under [PORTING.md v7](../PORTING.md)
R0–R1 and R9. Author route: `codex/gpt-6.1-sol@high` via Fabric. Command names
below are proposals for the command-tree freeze in `design/cli.md`.

## Purpose and boundary

`provenant-telemetry` answers how much supervised work used, which routes
succeeded, and how complete those measurements are. It stores local,
allowlisted usage and attempt facts, calculates exact API-equivalent cost
estimates, and produces a compact table or a typed JSON report. Execution
success is an outcome, never delivery acceptance. The ledger is advisory:
its failure cannot stop a provider, lose a terminal receipt or change routing.
Context occupancy, compaction and resume policy belong to **run**. Live quota
collection, scheduling, account identity and quota reports belong to **quota**.

## Evidence and feature decisions

Evidence at `1b3d48e4`, checked on 2026-10-23/24:

- Tool-call arguments in 1,401 Claude project JSONL files (485,046 records)
  and 1,044 Codex session files (329,245 records) yielded **four local
  `provenant stats --json` reads**, **five `telemetry sources --json`
  reads**, and two bare-script diagnostics in Provenant sessions. Matches
  span 2026-10-23 02:14–03:47 UTC. The scan excluded prose, searches, edits,
  heredoc bodies and test strings. No observed call used `--hosts`,
  `--compare`, `--detail`, `accepted`, `changes`, `retro` or explicit ingest.
  This sparse diagnostic sample is evidence of readers, not exhaustive
  absence or sustained use; stats previously refreshed implicitly.
- The documented local ledger, opened read-only, had zero `usage`,
  `deliveries`, `changes` and `events` rows, 150 `quota_series` and four
  `quota_events` rows; journal mode `delete`, no sidecars. This is one host.
- Callers were searched in skills, `scripts/hooks/`, scripts, docs,
  `tools/front-door/src` and Fabric. No skill/hook invokes a report. The
  [retrospect skill](../../../../skills/retrospect/SKILL.md) reads retained
  evidence directly; the [delivery reference](../../../../skills/deliver/references/contract.md)
  promises stats without a reader. The [telemetry](../../../runbooks/telemetry.md),
  [host](../../../runbooks/fabric-hosts.md) and
  [routing](../../../runbooks/model-routing.md) runbooks describe broader
  features; documentation alone does not establish use.

| Decision | Build requirement and evidence |
|---|---|
| Keep local usage/cost and outcomes | Four JSON report reads; receipts supply usage/route/status/timing. Keep provider/model and daily account totals, latest task outcome, attempts, retries, errors and latency samples. |
| Keep source health and ingest | Five source-health reads. Explicit ingest repairs missed advisory writes from new-state attempt records. |
| Keep old-ledger import | R1 permits it; history on other hosts may be useful. Read retained facts, never their referenced files. |
| Drop old transcript/receipt discovery | R1 forbids old-root reads. No Claude/Codex transcript, OpenCode DB or `.agent-run/` crawl. Unsupervised native CLI usage becomes **outside coverage**, not zero. |
| Drop delivery telemetry | Zero rows and no observed reader. Remove `accepted`, cost-per-accepted, writer/reviewer summaries and delivery/repair/finding joins. Workflow keeps acceptance evidence. |
| Drop changes/compare/retro | Zero change rows, invocations and skill consumers. Remove GitHub scraping, markers, statistical verdicts and attention proxies. The retrospect skill remains. |
| Drop cross-host summaries | No `--hosts` use; only telemetry's own `fabric_hosts.py::telemetry_summary` consumes them. Remove that operation; retain local host identity. |
| Drop OTel ingestion/collector/export mapping | Zero events or readers; raw collector payloads duplicate collection. Prompt/tool logging stays **off**, including length/decision counters. Remove source/install options; preserve stores and leave installed-service changes to authorised cut-over. |
| Drop telemetry quota history and rollout fallback | The 150/four historical rows prove collection, but no script/skill/report queries those histories. Quota keeps live snapshots and reset interpretation independently. Old histories remain untouched. |
| Drop shadow blobs/model heuristics | Routing docs suggest SQL inspection, but no actual read was found. Keep task class and exact model components; remove alias/version guessing and model-regex fixture detection. |
| Drop parent rollups/compaction telemetry | No reader found; run owns lineage/context. Retain parent references without another additive total. |
| Drop read-time refresh/top-level aliases | Read commands never write. Advisory run writes plus explicit ingest replace the hourly setting; only the `telemetry` subtree survives. |

The existing Rust crate and [closed PR #1340](https://github.com/mblauberg/provenant/pull/1340)
were read as source material. Reuse ideas about allowlisting, missingness and
idempotence. Replace the inert implementation, its float arithmetic,
`pending` records/Unicode helpers and Python/Node differential tests. None
of their output wording, sorting or runtime emulation is a contract.

## Proposed command subtree

All commands accept `--json`. Root/state selection is the global CLI/config
contract, not a telemetry-specific environment resolver.

| Command | Flags | Human output | JSON result payload |
|---|---|---|---|
| `provenant telemetry ingest` | none | New/unchanged/failed attempt-record counts and source status | `IngestReport`: counts, source status, bounded diagnostics |
| `provenant telemetry sources` | none | Source, record version, last attempt/success, committed rows, status | `SourceReport`: the same fields plus import receipt summary |
| `provenant telemetry stats` | `--since DATE_OR_RFC3339`, `--until DATE_OR_RFC3339`, `--detail` | Usage/cost and route outcome tables; detail adds daily account rows | `StatsReport`: usage, daily usage, outcomes, coverage, sources |
| `provenant telemetry import` | required `--from FILE` | Imported/suppressed/skipped row counts and immutable source fingerprint | `ImportReport`: schema, fingerprint, per-table counts, conversion warnings |

No telemetry MCP tools are proposed. No `snapshot` command, SSH telemetry
operation or MCP registration survives. Skills needing these figures call the
canonical JSON CLI; they do not open SQLite. Installation/help must remain
cheap and must not open the ledger.

`stats` defaults to all available history. Bounds select `[since, until)`;
dates mean midnight UTC, timestamps require an offset, `since >= until` is
invalid. Undated imported facts appear only without bounds, counted
separately. Every report declares `supervised_attempts` coverage plus any
`legacy_import`, never whole-account coverage.

Human tables show 12 rows per section (200 with `--detail`) and omitted
counts; missing values print `-`. JSON ignores detail, never silently
truncates, and refuses above 8 MiB with `report_too_large` and advice to narrow
the period. Sort by group keys/day, missing last. Diagnostics use stderr.

Each JSON response is one object:
`{ "schema": "provenant.telemetry.<result>.v1", "partial": false,
"result": ..., "diagnostics": [...] }`. Closed typed diagnostics have a
code, source kind and optional opaque record ID, never raw input or error
text. Source states are `not_ingested`, `ok`, `absent`, `partial`, `error`.
A failed file does not erase successful files; partial results are explicit.

Exit codes: **0** complete success (including empty state, absent sources and
an identical repeated import); **1** I/O, corruption, unsupported schema,
invalid input record, overflow, size/deadline limit, `already_imported`
with a different snapshot, or an ingest with any failed file;
**2** invalid CLI arguments; **3** writer contention or a changed/busy legacy
source; **4** a usable report with partial collection coverage. Missing
uninitialised state is an empty read with `not_ingested`, not a created DB.
An existing incompatible/corrupt DB is an error, not an empty report. Fatal
JSON errors use the same envelope with `result: null`; clap usage errors
remain CLI-owned.

## Records, layout and durability

Use the **new `StateRoot` supplied by config**, at the location chosen by the
state design. Do not honour old Fabric state-root variables or fall back to
old files. Below that root:

```text
telemetry/
  ledger.sqlite3          # private, schema version 1
  writer.lock             # supervisor-held, bounded acquisition
  import.snapshot         # one reserved temporary slot, never historical state
runs/...                  # new run-owned terminal attempt records; input only
```

No copies of prompts, transcripts, raw receipts or arbitrary JSON attributes
are stored. Directories are 0700 and files 0600. The new run layout is an API
request to execution/state, not permission to scan the old `.agent-run/`.

`provenant-records` owns the serialised models. Own records deny unknown
fields and carry `schema_version: 1`; absent optional measurements are null.
Unknown versions fail closed. Identifier strings are at most 256 UTF-8 bytes;
opaque core IDs retain their tighter type bounds. Reject oversized fields
before retaining them, including legacy projections. The schema is small:

| Record/table | Key and retained fields |
|---|---|
| `AttemptFact` / `attempts` | `(host_id, project_id, run_id, task_id, attempt)`; optional parent attempt, terminal time, exact adapter/provider/account, requested/resolved/observed model, applied effort, task class, route label, status/error-class enums, wall/provider milliseconds, fixture origin, optional `Usage` |
| `UsageFact` / `usage` | Stable source namespace and event ID; optional attempt/session reference, host/account/model identity, observation time, typed `Usage`; `live_attempt` or `legacy_native`/`legacy_receipt` provenance |
| `Usage` | Optional integer input-uncached, cache-read, cache-write, output-without-reasoning and reasoning tokens; explicit unknown-breakdown flags and usage origin; no provider-reported floating cost |
| `SourceState` / `sources` | Source kind (`attempt_records`, `legacy_import`), parser version, last attempted/successful UTC time, committed counts and bounded error codes |
| `ImportedLedger` / `imports` | One accepted snapshot fingerprint, source schema, importer version, imported/suppressed/skipped counts and numeric-conversion warnings |

The usage table is the sole additive token source; attempt facts supply
outcomes, never a second usage sum. A live attempt and its usage projection
commit together. Core/records IDs and canonical digests replace path/hostname identity.
Stable host IDs survive renames; project IDs prevent same-run collisions.
Report model is observed, otherwise resolved, otherwise requested, with its
basis. Missing identity stays unattributed; explicit fixture origin excludes
tests from production totals. No email/directory/model-suffix inference.

SQLite is the ledger and transactional index, using the already allowlisted
`rusqlite` with its existing `bundled` feature. Index observation time and
attempt identity. Persist typed record JSON with a 64 KiB per-record bound;
indexed timestamps are signed epoch milliseconds, counters inside records
are `u64`. Do not route unsigned counts through SQLite REAL or SQL SUM:
stream selected records and use checked Rust reductions. Before inserting a
group, enforce both 10,000 groups and 1 MiB cumulative retained key bytes;
fixed numeric accumulators have no per-event vectors. Output counting also
stops at 8 MiB. Violations return `report_too_large` without retaining the
extra key or allocating an oversized output buffer.

The first build PR adds **only supervisor's safe filesystem/lock API** to
telemetry's R2 internal dependency row, alongside core/records/config, and
updates its layering check. This is a proposed subsystem edge change allowed
by R2, not an additional external crate/feature under R6. Use verified private
directories for SQLite's path-based open; reject symlinked entries and an
untrusted/writable-by-others directory. Supervisor owns all unsafe code,
verified file handles, locks and atomic file publication. Telemetry gets no
process-control capability.

Use rollback-journal mode with `synchronous=FULL`, one connection per owning
thread and one writer lock for ingest/import/advisory writes. Acquisition is
nonblocking; SQLite waits at most 250 ms. No transaction spans a provider,
process, network or user wait. Initial DB publication is atomic and fsync'd
through supervisor. Schema changes, each source's successfully ingested fact
and its checkpoint, and the import marker with all imported rows are committed
atomically in SQLite. Normal reports open read-only and perform no migrations,
refreshes, retention or journal maintenance.

Run publishes terminal state durably **before** advisory telemetry.
`record_attempt` is idempotent; conflicting payloads for one terminal identity
fail. Ingest reads only bounded (64 KiB), verified new-root terminal records,
never embedded paths; each successful record commits independently. Missing
files are not deletions. Report last reconciliation and known ingest errors;
it cannot prove every attempt was collected. A failed advisory write is
repairable from the retained terminal record.

## Attribution, cost and import

For new work, sum every attempt's usage, including retries; a task's latest
attempt determines its outcome. Route groups use the exact route actually
recorded on each attempt. Report task success once, under its latest route;
keep earlier routes' attempt counts and failures separately. Parent/child
relations are references, not another amount to add to overall totals.

Providers owns normalisation exactly once: subtract known cached/write input
and reasoning output where its wire form is inclusive. Run supplies the
resulting disjoint categories; telemetry validates them without subtracting
again. An inconsistent breakdown is a typed invalid measurement. If a required category is unknown,
keep the breakdown and cost unknown rather than clamping or inventing zero.
Report each known token sum with its missing-row count. Cache share is an
exact numerator/denominator over rows with complete input categories, with
the included-row count. Percentiles use nearest rank over integer milliseconds
and expose sample counts; do not average percentiles or manufacture throughput
from missing provider time.

Retain the reviewed price asset at `config/telemetry-prices.json`, converting
its schema to integer **USD microdollars per million tokens**, null for an
unverified rate, with reviewed date, source references and explicit
free/subscription/API billing. Config supplies the validated table; no runtime
price fetch or catalogue heuristics. Exact model keys and explicit aliases
in that asset select a price. No inferred free-model patterns. Use checked `u64` quotient/remainder
arithmetic with denominator D=1,000,000: for tokens t and rate r, accumulate
`(t/D)*r + (t%D)*(r/D) + ((t%D)*(r%D))/D` whole microdollars and retain
`((t%D)*(r%D))%D` remainder. Each remainder product is below D². Carry
remainders across buckets/rows and round half-up once at final output;
never form t*r, which can overflow despite a representable result.
Reasoning uses the output rate once. Final count/money overflow is a typed
error. JSON money is a six-place decimal **string**, never `f64`.

An aggregate estimate is null if any included row lacks a required token
category or positive-count price. Expose known-cost lower bound and missing
cost rows separately. A zero count needs no rate; zero total is not evidence
that a subscription or unpriced model is free. Cost per successful task is
null if any contributing attempt is unpriced or no task succeeded. It is an
execution metric, not cost per accepted delivery.

`telemetry import --from FILE` is the sole exception to fresh-state reads:

1. Accept an explicit, regular, non-symlink SQLite **schema-4** ledger, at
   most 1 GiB. Do not discover it, open old config, follow `source_ref` or
   usage-file paths, or rescan any transcript/receipt. Other schemas are a
   typed refusal; there is no old-schema migration engine.
2. Require a quiescent main file: refuse any journal/WAL/SHM sibling. Pin
   the regular source file with supervisor; hash it, copy from that same
   handle into a private new-root temporary snapshot, and require the copy's
   digest and a final source digest to match. Recheck source identity/size
   and sidecar absence; changes return 3 without a destination commit.
   The copy uses the single reserved `telemetry/import.snapshot` slot, under
   the writer lock. Before each import, supervisor removes a stale regular
   scratch file from that slot and creates it exclusively; no directory sweep
   or Drop-based cleanup. Thus retries cannot accumulate copies. Reject a
   source inside StateRoot or aliasing the destination/scratch file before
   cleanup. No source checkpoint, recovery, lock/write or config read.
3. Open **only the verified private copy**, using SQLite
   `mode=ro&immutable=1` and `trusted_schema=OFF`, never reopen the old path
   through SQLite. This closes replace-and-restore races. Unlink the private
   slot after the immutable connection opens; its held SQLite descriptor
   keeps the snapshot alive and the OS reclaims it even after a kill. A crash
   before unlink leaves at most one scratch file, removed before retry.
   Never unlink the source. Bound each row and the import to 60 seconds;
   failure rolls back. A busy legacy store
   needs a quiescent copy supplied by its owner, not importer repair.
4. Project only `usage` facts, attempt metadata from `source=fabric`, and
   host alias metadata necessary to deduplicate that ledger's own rows.
   Read a fixed column allowlist, allowing schema-4 additive columns to be
   absent. Required missing columns or conflicting duplicate identities fail
   the transaction. Do not import cursors, arbitrary events, deliveries,
   changes, quota histories, source paths or classifiers. Report their
   skipped row counts explicitly; they remain available in the old ledger.
5. Legacy integer token fields must be nonnegative SQLite INTEGER values
   within `u64`; reject malformed retained rows, never round token counts.
   Drop old REAL costs/confidences rather than emulate their floats;
   recompute estimates from integer token fields and the reviewed price table.
   Mark ambiguous old bucket breakdowns unknown. Preserve unknown model
   components rather than invent requested/resolved values from the folded
   field. Skip nonterminal receipt outcomes; unfamiliar terminal statuses become
   `legacy_unknown`. Keep their usage coverage separate from known outcomes.
6. For an imported `(host, adapter, account, session)` with native usage,
   suppress receipt token totals for that **whole imported session**, retaining
   receipt outcomes. Otherwise use receipt usage. Null session IDs never
   match. Collapse repeated `(source,event_id)` across the ledger's recorded
   local host aliases; preserve other hosts. `meta.host_ids` supplies aliases,
   not a current-host ranking: identical projected facts collapse, conflicting
   facts fail without guessing or consulting old host config. Fix this choice
   at import, independently of later date filters, and expose suppressed-row
   counts and partial native coverage. Never sum native and receipt totals.
7. Commit all projections and the import fingerprint in one destination
   transaction. An identical retry is a no-op; a different snapshot after
   successful import is refused as `already_imported`. No merge/update mode,
   destructive reset or source deletion exists.

## Crate interfaces and dependencies

Proposed APIs, to settle with their owners before build:

| Telemetry API | Caller/contract |
|---|---|
| `record_attempt(store, &AttemptFact) -> Result<WriteOutcome>` | Run, after durable terminal publication; bounded failure is diagnostic, never a stop reason. |
| `ingest(store, AttemptRecordReader, now) -> Result<IngestReport>` | CLI; run supplies verified new-root records for repair. |
| `stats(store, StatsQuery, &PriceTable) -> Result<StatsReport>` | CLI rendering; no SQLite access by skills. |
| `sources(store) -> Result<SourceReport>` | CLI diagnostics, read-only. |
| `import_legacy(store, VerifiedLegacyFile, deadline) -> Result<ImportReport>` | CLI's explicit one-shot import. |

Typed errors: `Busy`, `Io`, `Corrupt`, `UnsupportedVersion`, `InvalidRecord`,
`ConflictingFact`, `Overflow`, `TooLarge`, `DeadlineExceeded`, `LegacyChanged`,
`AlreadyImported`. Busy/LegacyChanged map to 3; all other listed errors to 1.
**Records/core** supply versioned facts, IDs, canonical encoding, timestamps
and deadlines. **Run/providers** supply normalised optional usage and exact
route/account/model provenance, without excerpts. **Config/state** supply
StateRoot, stable host identity, private-directory ownership and validated
prices; they own replacing legacy account configuration. **Supervisor**
supplies safe handles, locks/publication and legacy-file verification.
**CLI** supplies clocks/deadlines and renders exits/output. **Quota** keeps
live snapshots independently; Fabric/workflow require no telemetry API.
Telemetry imports none of run/quota/providers/workflow/fabric-store;
`run -> telemetry` remains the dependency direction.

## External contracts and fixtures

No direct provider argv, hook, Git or MCP protocol is implemented here.
Their owners retain R0 contracts; owned command removals do not change MCP
itself. Keep external `SKILL.md` frontmatter intact and update surviving
report references with their owners.

Provider fixtures must pass through **providers**' normaliser into telemetry,
not a second parser. Recorded source material includes
`tests/fixtures/fabric-context/{claude.jsonl,codex-rollout.jsonl,opencode.jsonl}`
and `tests/fixtures/fabric-v1/provider-{claude,codex,cursor,agy,opencode}.json`.
Reuse verified fragments with capture version/provenance; synthetic attempt
records and Python-generated oracles are not external recordings. Missing
usage-shape recordings are a provider-lane prerequisite collected with
appropriate authority. Telemetry tests make no live provider calls.

For the **one legacy-input contract**, check in a sanitised schema-4 SQLite
fixture made by SQLite tooling with the documented columns and representative
retained rows, plus a capture manifest of a real old ledger's column/types.
Cover additive/missing optional columns, mixed native/receipt sessions,
host aliases, unknown models and REAL cost fields. Use independent expected
facts and integer arithmetic; do not execute Python to decide results.
No personal transcript, email, prompt or credential bytes enter fixtures.

## Tests and acceptance

Follow R9; delete removed-subject tests instead of translating them.

- **Unit, beside modules:** exact rounding/final overflow and representable
  products exceeding u64 (10¹² tokens × 10⁷ rate), unknown versus free
  prices, missing buckets/coverage, UTC/undated bounds, exact model identity,
  route-changing retries/latest task outcomes, nearest-rank samples,
  aliases/sessions/fixtures, including host_ids-only conflicting aliases,
  immutable-fact conflicts, versions and bounds, including many oversized
  distinct keys rejected before aggregation allocation.
  Inject clocks and data.
- **Binary integration, isolated state:** real CLI human/JSON shapes/exits;
  empty reads create nothing; corruption refuses; ingest repairs advisory
  failures; repeat ingest/import deduplicates; source bytes/sidecars stay
  unchanged; imported native/receipt selection is independent of filters;
  oversize input/report refuses. Secret sentinels in discarded receipt fields
  must never reach DB/output. Old-root tripwires permit only the explicit
  import file and its sidecar checks.
- **Process, Linux/macOS:** handshake/FIFO fixtures hold the writer lock and
  verify bounded code 3, coherent reads, interrupted-import rollback/retry,
  ENOSPC/fsync failure without partial facts/marker, stale-copy reclamation
  after termination during copy/open and before retry, and source replacement/
  symlink refusal, including replace-and-restore before SQLite open. A fake
  provider remains alive under telemetry failure,
  publishes terminal once and leaves ingestible evidence. No sleeps;
  canonicalise macOS temporary paths.

Build locally with fmt/clippy/targeted tests; CI runs workspace Linux and
macOS release/process checks. This docs-only PR uses link/scope/whitespace
checks and native review; the chair owns cross-family council acceptance.

## Deletion inventory and line budget

Physical line counts at `1b3d48e4`; partial ranges name the baseline only.
Every listed deletion lands with its Rust replacement or the Rust command/
installer integration that removes that surface. Do not delete stores,
transcripts, installed files or another lane's whole shared module.

| Python file or test/helper | Lines removed |
|---|---:|
| `scripts/telemetry.py` | 1,027 |
| `scripts/telemetry_ingest.py` | 476 |
| `scripts/telemetry_costs.py` | 263 |
| `scripts/telemetry_report.py` | 396 |
| `scripts/telemetry_changes.py` | 406 |
| `scripts/telemetry_delivery.py` | 276 |
| `scripts/telemetry_hosts.py` | 207 |
| `tests/test_telemetry.py` | 2,967 |
| `tools/front-door/crates/telemetry/src/pending/python/generate_unicode.py` | 29 |
| `tests/test_otlp_collector.py` | 213 |
| `tests/test_install_harness.py`, OTel helpers/tests, lines 38–124 only | 87 |
| `tests/test_dispatch_run.py`, telemetry-only assertions, lines 6958–6967 only | 10 |
| **Python total** | **6,357** |
| **Telemetry-owned TypeScript files** | **0** |

Replace all 13 current telemetry `.rs` files, **5,810 lines**, including the
749-line generated Unicode table, the 593-line differential harness and all
`pending` helpers. No `context.rs` from closed PR #1340 is adopted; context
ownership stays in run. Budget the new telemetry production code plus unit,
integration and process tests at **at most 3,000 Rust lines**: expected net
Python/TS/Rust change is **at most −9,167 lines** (3,000 − 6,357 − 5,810).
This is a design budget, not measured implementation savings; report actual
`Net lines:` in each build PR and justify growth.

Also retire `scripts/provenant-telemetry` (5 shell lines),
`scripts/install-otel-collector` (46 shell),
`tools/otlp-collector/otlp-collector.swift` (199), its
`com.provenant.otlp-collector.plist.template` (26), the four
`schemas/telemetry-{usage,event,quota-series,quota-event}-row.schema.json`
files (298/110/110/83), and the two `tests/golden/cases/telemetry/*.json`
files (12 each). These **901 additional lines** are outside the language
budget; new recorded wire fixtures are separately measured.

Shared-owner cleanup below is part of those owners' Rust PRs. Sizes are
whole-file baseline lines for identification, **not deletion credits**:

| Shared file | Current lines | Telemetry coupling removed/replaced |
|---|---:|---|
| `scripts/provider_quota.py` | 1,611 | `_telemetry` (8), `state_root` (2), `ledger_windows` (21), `record_series` (15); remove DB writes from `record_events`, retain reset interpretation. |
| `scripts/quota_providers/codex.py` | 174 | Ledger fallback in `fetch`; retain app-server quota. |
| `scripts/quota_display.py` | 399 | Telemetry table-renderer import in `render`. |
| `scripts/fabric_hosts.py` | 807 | `telemetry_summary` (19) and verb registration. |
| `scripts/utility.py` | 241 | `record_call` (11) and its telemetry import/calls. |
| `tests/test_host_identity.py` | 129 | Telemetry subject only. |
| `tests/test_provider_quota.py` | 2,188 | Removed history/fallback/peer-summary subjects only. |
| `tests/test_record_schemas.py` | 387 | Telemetry schemas/subjects; registry retires per the plan. |
| `tools/front-door/crates/quota/src/tests/oracle.py` | 84 | Telemetry renderer import; whole Python oracle retires with quota. |
| `scripts/install-harness` | 702 | Collector flags/wiring; other installation belongs to state/CLI. |

These shared files are not wholesale telemetry deletions. Their owners count
actual removed lines once. Retain unrelated quota/identity/dispatch/peer
tests; add no adapter or broken-test shim while owners wait (R10).

Replace front-door dispatch/help entries for `telemetry`, `stats` and `retro`
with the proposed subtree. State/CLI owns removal of collector installer
flags/wiring in `scripts/install-harness` and its shared tests. Update the
telemetry/host/routing runbooks and the surviving delivery/routing skill
references. Keep the price asset, changing its number schema with its reader.

## Build order

1. **Local ledger and reports:** land the records, exact price reader,
   transactional telemetry store, ingest/report/import APIs and CLI subtree
   together, with the Rust tests above. Update R2's supervisor-safe-API edge
   and layering check in this PR. Delete the seven telemetry Python modules,
   wrapper, primary tests, old schemas/goldens and all inert telemetry Rust
   and its generator. This is one coherent subsystem replacement, targeted
   at the 3,000-line Rust budget; it is usable against fresh records and the
   legacy fixture without a provider or a Python caller switch.
2. **Run publication integration** (execution lane): land run's typed terminal
   projection and bounded advisory call, using the already live telemetry
   API; delete the old dispatch telemetry assertions and utility event
   callback with their owners. Prove bookkeeping failure cannot affect
   custody/publication. Real new-root attempts then populate the ledger;
   the ingest repair path remains available.
3. **Owner cut-over integration** (state/CLI, quota and Fabric lanes): land
   their Rust installer/commands/quota/peer owners and delete the collector,
   installer helper/flags/tests and remaining shared Python telemetry calls
   named above. Remove telemetry peer/retro/acceptance promises from callers
   in those same PRs. No Rust collector or replacement retrospective service
   is built. Each owner's PR reports its own deletions without double credit.

The workshop chair resolves the requested records/run/filesystem interfaces,
reviews this proposal with the cross-family council, and freezes commands
with the CLI owner before step 1. Build lanes open PRs; they do not merge.
