# Telemetry port batch 1

Rulebook: PORTING.md version 2. Source and policy-test oracle:
`e897b10d58b27a46544969594eed08a5ed64c72c`.

The crate is inert. No command, stable installation, live database or source
implementation changes owner in this batch. Callers supply identity, clocks,
price/catalogue data, verified streams and persistence through explicit values
and sink traits. A sink stamps the current host and applies the existing ledger
ownership, deduplication and event-ID rules before retaining parser output.

## Unit disposition

| Unit | Status | Delivered | Remaining |
|---|---|---|---|
| `scripts/telemetry.py#interface` | Complete interface | Explicit identity, usage/event/observation contracts, typed errors and sink boundary | Runtime/schema owner is outside this interface task |
| `scripts/telemetry_changes.py#unit-01` | Partial | Windows, whole-line UTF-8 budget, statistical verdicts and ledger attention proxies | GitHub transport and durable range sync, markers, pivot resolution, metric-side composition, comparison assembly/rendering |
| `scripts/telemetry_costs.py#unit-01` | Partial | Catalogue/model attribution, prices, missingness, exact counter aggregation and final cost rounding | SQL usage-cell grouping and cell reducers |
| `scripts/telemetry_delivery.py#unit-01` | Partial | Receipt metadata, earliest receipt clock, repair transitions and hashes, review lineage, safe compaction metadata, compaction SQL report and explicitly linked accepted-delivery costs | File discovery, receipt/sidecar cursor invalidation and SQLite persistence, delivery report selection and accepted-cost text output |
| `scripts/telemetry_hosts.py#interface` | Partial, blocked | Read-only peer transport request/reply, status and validated v1 aggregate contracts | Exact lexical peer integers beyond u64 need a shared record codec (RP-1013); SSH collection, snapshot validation and host merge belong to the later implementation unit |
| `scripts/telemetry_ingest.py#unit-01` | Partial | Claude/Codex allowlisted parsers, cumulative reset policy, quota observations, OTLP metadata privacy and record multiplicity, snapshot-bound JSONL rewrite/append engine, millisecond stamps | Verified no-follow file opening and the SQLite cursor/clear-file adapter |
| `scripts/telemetry_report.py#unit-01` | Partial | Outcome aggregation, nearest-rank percentiles, latest-attempt selection, peer outcome projection, compact numbers and bounded tables | Ledger stats selection/deduplication, full peer summary and quota snapshot projection |

The five implementation units exceed this slice when their database, transport
and cyclic owner integration is included. Their delivered policy parts remain
independently usable and testable. They are not listed as fully ported units.
No graph statuses are changed.

## Pending ownership

`src/pending.rs` and its private numeric module contain temporary contracts:

- `Usage`, `Event`, `Observation`, `Numeric`, Python-compatible JSON/hash encoding
  and hex encoding belong in `provenant-records`.
- ISO timestamp parsing and rendering belong in `provenant-core`.

The current `core`, `records` and `config` libraries are scaffolds at the oracle
revision. No unavailable sibling API, dependency cycle or second runtime owner
is introduced. Verified file opening is deliberately not approximated with a
path check followed by an ordinary open.

## Verification and limits

Local gates: `cargo fmt --all`, `cargo clippy -j 2 -p provenant-telemetry
--all-targets -- -D warnings`, `cargo test -j 2 -p provenant-telemetry` with the
pinned toolchain. 29 tests pass, including four Python oracle comparisons.
Python-dependent tests report a skip and return only if `python3` is absent.
The comparisons run the checked-out source on isolated metadata fixtures,
never the installed front door or a live provider.

Policy evidence comes from `tests/test_telemetry.py`: missing usage/prices,
reasoning billing, model aliases and namespaces, sample sizes/noise, repair
transitions, compaction privacy, explicit delivery lineage, OTLP field
allowlists/multiplicity, partial lines/rewrites, peer numeric validation and
latest-lane outcome/latency/throughput rules. Regression fixtures also cover
Unicode hashes, scientific float spelling and integer counters above 2^53.

One native adversarial source review and repair reviews were performed for this
batch. Three original findings are repaired and recorded in the divergence log.
A remaining lexical-integer issue is open as RP-1013 and blocks completion of the
peer interface. This PR remains a draft; no unit/runtime promotion is proposed. Cross-family review,
independently owned old-pass/mutant-fail receipts, J1 golden and full J2 conformance
are not claimed. `tests/golden` is absent at this source revision. Those gates and
exact-head CI remain requirements before integration/landing acceptance. Nothing
in these targeted tests establishes complete unit, runtime or cut-over parity.

No intentional behaviour changes are proposed. The crate's tests are the
surviving regression evidence; Python policy tests are retained.
