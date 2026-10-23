# Telemetry port batch 1

Rulebook: PORTING.md version 4, R4/R9, from PR #1297
(`41b1219cc3fb1a3febc0a47a851dc57d165bee78`). Source and policy-test oracle:
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
| `scripts/telemetry_changes.py#unit-01` | Partial | Windows, whole-line UTF-8 budget, statistical verdicts and ledger attention proxies | Ledger query adapter, GitHub transport and durable range sync, markers, pivot resolution, metric-side composition, comparison assembly/rendering |
| `scripts/telemetry_costs.py#unit-01` | Partial | Catalogue/model attribution, prices, missingness, exact counter aggregation and final cost rounding | SQL usage-cell grouping and cell reducers |
| `scripts/telemetry_delivery.py#unit-01` | Partial | Receipt metadata, earliest receipt clock, repair transitions and hashes, review lineage, safe compaction metadata, compaction report policy and explicitly linked accepted-delivery costs | Ledger join/query adapter, file discovery, receipt/sidecar cursor invalidation and SQLite persistence, delivery report selection and accepted-cost text output |
| `scripts/telemetry_hosts.py#interface` | Complete interface | Typed peer request/reply, status, summary and transport/error contracts | Implementation belongs to `#unit-01` |
| `scripts/telemetry_hosts.py#unit-01` | Partial | v1 summary validation | SSH collection, snapshot validation and host merge |
| `scripts/telemetry_ingest.py#unit-01` | Partial | Claude/Codex allowlisted parsers, cumulative reset policy, quota observations, OTLP metadata privacy and record multiplicity, snapshot-bound JSONL rewrite/append engine, millisecond stamps | Verified no-follow file opening and the SQLite cursor/clear-file adapter |
| `scripts/telemetry_report.py#unit-01` | Partial | Outcome aggregation, nearest-rank percentiles, latest-attempt selection, peer outcome projection, compact numbers and bounded tables | Ledger stats selection/deduplication, full peer summary and quota snapshot projection |
| `scripts/telemetry_report.py#unit-02` | Complete | Stats text renderer, including summary/detail tables and omission notices | None |
| `scripts/telemetry_changes.py#unit-02` | Skipped | None | Depends on the unfinished comparison/retro report assembly in unit-01 |
| `scripts/telemetry_ingest.py#unit-02` | Skipped | None | SQLite OpenCode snapshot/watermark ingestion and shared cursor/file operations belong to the store and supervisor owners, not yet available in this checkout |
| `skills/orchestrate/scripts/context_usage.py#unit-01` | Skipped | None | Reads provider context events plus Codex rollout files, environment, product config and argv; the crate has no established context/dispatch configuration API |
| `skills/orchestrate/scripts/context_usage.py#unit-02` | Skipped | None | Depends on the missing context meter and resume/dispatch contracts from unit-01 |

This batch completed only `scripts/telemetry_report.py#unit-02`. The listed
cost, delivery, changes, ingest and report unit-01 work remains partial where
batch-1 already delivered pure policy; this batch adds no SQLite, GitHub,
verified-file or command integration. The other batch-2 units are skipped for
the dependency reasons above. No intended behaviour changes or new divergences.

The implementation units exceed this slice when their database, transport
and cyclic owner integration is included. Their delivered policy parts remain
independently usable and testable. They are not listed as fully ported units.
No graph statuses are changed. An interface claim covers types, traits and
errors only; identity helpers, validators and other behaviour belong to the
corresponding implementation units and do not imply their completion.

## Pending ownership

`src/pending.rs` and its private modules contain temporary contracts:

- `Usage`, `Event`, `Observation`, `Numeric`, Python-compatible JSON/hash encoding
  hex encoding and checked JSON decoding belong in `provenant-records`.
  External bytes cross `decode_json` before typed decoding: lexical integers
  outside i64/u64 return `Error::NumberOutOfRange` (RP-1013).
- ISO timestamp parsing/rendering and CPython float-second conversion belong
  in `provenant-core`. The converter splits whole/fractional seconds and uses
  half-even microsecond rounding.
- Python compatibility primitives use the verified CLI digit/strip/`repr`
  helpers and Unicode 16 printable table, extended in `pending` for letter
  classification, JSON-value stringification, truthiness and numeric equality.
  They belong in `provenant-core`. The exhaustive letter fixture uses the
  canonical CPython 3.14 / Unicode 16 baseline; older accepted interpreters may
  expose a Unicode database mismatch rather than skip that comparison.

The current `core`, `records` and `config` libraries are scaffolds at the oracle
revision. No unavailable sibling API, dependency cycle or second runtime owner
is introduced. Telemetry has no direct SQLite dependency: `attention` consumes
typed events and the compaction report consumes typed joined rows. Ledger
query execution belongs to the store owner. Verified file opening is deliberately not approximated with a
path check followed by an ordinary open.

## Verification and limits

Local gates: `cargo fmt --all`, `cargo clippy -j 2 -p provenant-telemetry
--all-targets -- -D warnings`, `cargo test -j 2 -p provenant-telemetry` with the
pinned toolchain. 55 tests pass, including nine Python oracle comparisons,
with `PROVENANT_PORT_DIFF_REQUIRED=1`, Python 3.14.8, default macOS `TMPDIR`
and a temporary `CARGO_HOME` because the global cache is read-only.
The crate's single oracle helper honours `PROVENANT_PORT_PYTHON` (default
`python3`, >=3.12) and `PROVENANT_PORT_NODE` (default `node`, >=24); this crate
currently compares Python sources only. Missing/old interpreters emit `skip:`
and fail when differential execution is required. Floor-boundary and missing
interpreter fixtures cover that policy. Numeric comparisons include exact
spelling. The native-parser fixture canonicalises its temporary directory
before supplying the same path to Rust and Python.

Policy evidence comes from `tests/test_telemetry.py`: missing usage/prices,
reasoning billing, model aliases and namespaces, sample sizes/noise, repair
transitions, compaction privacy, explicit delivery lineage, OTLP field
allowlists/multiplicity, partial lines/rewrites, peer numeric validation and
latest-lane outcome/latency/throughput rules. Regression fixtures also cover
Unicode hashes, scientific float spelling and integer counters above 2^53.

The original seven blocking findings have regression coverage: integer/hash
spelling, all Event fields plus refresh, fractional timestamp rounding, huge
reset integers, price selection order, missing versus null IDs and integer
active-time output. Follow-up regressions cover Unicode windows/refusal repr
arithmetic outside the i64/u64 wire range, model whitespace/digits/aliases,
numeric cost-group equality and sorting, trailing table whitespace and exact
active-time integers above 2^53. The unverified WIP commit
`3e17af47` passed all 48 original tests before these additions; its formatting
failure is repaired.

A fresh native adversarial re-review completed after the follow-up repairs,
with no remaining actionable findings.
Cross-family review and independently owned old-pass/mutant-fail receipts are
not claimed. J1 golden and full J2/runtime conformance remain cut-over gates;
the merged main now supplies the golden runner. Exact-head CI gates landing.
The crate remains inert and partial implementation units remain unclaimed.

The v4-approved typed refusal beyond i64/u64 is the intentional `improvement`
divergence recorded in RP-1013. Python policy tests are retained.
