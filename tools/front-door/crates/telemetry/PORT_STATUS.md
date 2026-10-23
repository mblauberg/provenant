# Telemetry port batch 3

Route: `codex/gpt-6.1-sol@high via Fabric`. Rulebook: PORTING.md version 4.
Source/oracle snapshot: `7021182d535f6c730c29713e77d5bca76893fd06`;
subsequent main merges do not change these Python sources. The crate remains
inert: no command, source runtime, live ledger or stable installation changes
owner. The installed `provenant root` remains `/Users/mikey/Repos/provenant`.

## Unit disposition

| Unit | Status | Work in this batch | Remaining |
|---|---|---|---|
| `scripts/telemetry_changes.py#unit-01` | Complete | SQLite change/meta/event store; durable bounded sync; GH/git command/environment/error policy; markers, pivots, side/comparison assembly, rendering and attention | Actual bounded utility execution is an injected owner seam, not a new process owner |
| `scripts/telemetry_costs.py#unit-01` | Partial, shared API blocked | SQL usage cells, alias deduplication, null/positive reducers, ordered group serialization and exact byte differential | Shared `provenant_records::checked_json` must preserve lexical float rounding; RP-TEL-JSON-FLOAT blocks the complete-unit claim |
| `scripts/telemetry_delivery.py#unit-01` | Partial | Concrete SQLite cursor/delivery/report/accepted-cost adapter; receipt and sidecar policy; discovery/read contracts; output and malformed-input differentials | A permitted adapter to supervisor safe filesystem/discovery APIs; R2 excludes a telemetry-to-supervisor dependency |
| `scripts/telemetry_ingest.py#unit-01` | Partial | Concrete SQLite cursor/clear-file adapter and verified JSONL seam; shared semantic owners | Concrete supervisor-backed no-follow adapter is outside the current R2 edges; input-limit decision remains open |
| `scripts/telemetry_report.py#unit-01` | Partial | SQL selection, bounds, deduplication, native suppression, attribution, limits, quota and delivery projection, peer summary and redaction | Ordered typed report/public-snapshot encoding; current Value reports sort object keys |
| `skills/orchestrate/scripts/context_usage.py#unit-01` | Partial | All adapter meters, rollout search/enrichment, streaming reads, model/ceiling policy, environment/store/provider seams, Unicode float semantics | Oversized rollout-line behavior needs a source-compatible resolution or acceptance; RP-TEL-INPUT-BOUND |
| `scripts/telemetry_changes.py#unit-02` | Complete | Retro assembly and retro/change text rendering | None |
| `scripts/telemetry_ingest.py#unit-02` | Partial | Document effects, SQLite/OpenCode snapshots and watermarks, streaming production row visitation, typed source/runtime orchestration | Concrete filesystem/registry/transaction adapter; partial API contracts require the source's per-file transaction boundary |
| `skills/orchestrate/scripts/context_usage.py#unit-02` | Complete | Marker and resume-warning policy | None |

The merged `scripts/telemetry_report.py#unit-02` renderer is repaired too:
nullable raw cells, empty identifiers, column omission, missing versus null
compaction hosts, falsy host reasons, and typed refusal for null quota numbers.
Differentials cover more than 12 rows and summary/detail variants.
Only complete units are appended to `ported.txt`; the other rows above are
coherent partial ports, not complete-unit claims.

## Shared and pending owners

The crate now depends directly on workspace `provenant-core` and
`provenant-records`. Python truthiness, string/repr/equality/strip and CPython
float-second conversion use `provenant_core::py`. The JSON bound/error adapter
uses `provenant_records::checked_json`; the old checked decoder, Python helper
module, Unicode printable table and generator are deleted.

Pending helpers still have no corresponding public owner API on the source
snapshot: telemetry Usage/Event/Observation records, exact Numeric arithmetic,
Python sorted JSON/hash/hex encoders belong in records; decimal digits and
letter classification, UTC text rendering belong in core. Timestamp wrappers
only adapt the shared core owner to telemetry errors and UTC-naive policy.
Filesystem traits name the safe-owner contract; supervisor's safe filesystem
API does exist on main, but the telemetry edge is not permitted by R2.
Utility execution, quota projection, source discovery and host identity are
injected at existing owner seams. No unsafe code or sibling owner copy is added.

## Verification and review

Pinned Rust toolchain and Python 3.14.8; isolated `-I -B` oracle with explicit
environment and bounded child observation. `cargo fmt --all` and
`cargo clippy -p provenant-telemetry --all-targets -- -D warnings` pass.
`PROVENANT_PORT_DIFF_REQUIRED=1 cargo test -p provenant-telemetry` currently
reports **118 passed, 1 failed**: the pre-existing cost/outcome differential
exposes RP-TEL-JSON-FLOAT after shared-owner adoption. No tolerance, skip,
private decoder restoration or expected-value change masks it. The shared
owner request was sent to the chair through Fabric; its fix is outside this
lane's write scope. Exact-head CI remains required before landing.

Native adversarial source review found and repaired raw SQLite binding,
Python truthiness/numeric equality, malformed-input effects, SQLite scalar
coercion, insertion-order float sums, significant-digit formatting, streaming
and fixture issues. Source checks rejected false-positive review claims about
non-object git_revision and the document validation wrapper. The chair owns
cross-family review and promotion; auto-merge is not armed here. J1/J2/J3 and
platform/cut-over judges remain outside this inert library batch.
