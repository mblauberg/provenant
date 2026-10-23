# Routing port status

Rulebook: PORTING.md v4 / ADR 0028. Inert library; no command changes owner.
Author route: `codex/gpt-6.1-sol@high`, as supplied by the Fabric worker contract.
Source and policy oracle baseline: `7475ff01e437ec51c760b852516c21a5281cf9c6`.
Python differentials run through `oracle.rs` using `-I`, an explicit isolated
HOME/state environment, file-backed streams and bounded child observation.
The local oracle is CPython 3.14.8; interpreters must be at least 3.12.

## Completed in this batch

- `scripts/model_route_live.py#unit-02`: fallback generation, OpenCode discovery
  deadline/cache decisions, and bare-name resolution, including ambiguity,
  suggestions and Python SequenceMatcher's autojunk policy. The discovery
  function sends the exact adapter/executable/deadline/listing-only/refresh
  request to a synchronous provider-listing callback and takes an explicit
  monotonic clock; it does not launch a provider.
- `scripts/model_route_pools.py#unit-03`: exact human listing rendering, ordered
  routes/synonyms, Unicode padding, hidden/cooling summaries and quota caveats.

Completion describes each unit's implementation, not a command cut-over. The
injected discovery edge still needs the process probe owner before use by a CLI.

## Continued partial units

- `scripts/model_route_catalog.py#unit-01`: all batch-1 validators retained;
  malformed-role/override differentials and typed regex-error propagation added.
  Still blocked by RP-130402: Python's expanding set-based alias candidates can
  depend on the iteration order of multiple adapters sharing one family. This
  batch does not choose a new policy or claim that mismatch is accepted.
- `scripts/model_route_live.py#unit-01`: `matching.rs` ports registered names,
  default/ambiguity precedence, retired GPT identities, family/version matching,
  live inheritance, suffix efforts, owner inference and training/free policy.
  `live.rs` adds cache filtering, cooldown normalization/coverage and recent
  failures with explicit clocks. Remaining: filesystem readers, quota adapter
  integration, and ordered suffix-level ingestion when multiple levels have the
  same suffix. No global mutable caches or Python loader seams are retained.
- `scripts/model_route_pools.py#unit-01`: resolve/split model, privacy traits
  (including inherited live traits), and spread family now join the existing
  validation/migration helpers. Remaining: migrate/reject ordered ingress and
  Python numeric equality for conflicting renamed values. Existing exact weight
  behavior follows the already-approved RP-130403 improvement.
- `scripts/model_route_preferences.py#unit-01`: loading/statuses and ordered
  unsupported-policy representation now join batch-1 admissibility/ranking.
  Non-string route statuses use Python spelling. Remaining: ordered candidate
  ingress (including nested non-string status representation), typed candidate
  refusal API and CLI-owned default-path application for the selection parser.
- `scripts/model_route_probe.py#unit-01`: model-list parsers for structured/plain
  listings, Kiro evidence predicate and duplicate-rejecting JSON loader added.
  Remaining: process probes, cache locks/publication, Kiro read-only chat and
  executable/version-bound metadata. No live provider is run by this batch.
- `scripts/model_route_probe.py#unit-02`: snapshot loading/refusals, source and
  Claude provenance trust, exact age cutoffs, effort checks, case-folded identity
  collision checks, and normalised-key insertion order are implemented and
  differentially tested. Remains partial because family alias validation inherits
  RP-130402. The ISO reader in pending is also awaiting its core owner.

## Skipped implementation units

- Both `scripts/model_route_cli.py` units: parser/health records from batch 1
  remain; command execution depends on the unfinished route resolver and owners.
- `scripts/model_route_pools.py#unit-02`: availability, random council, rotation,
  pick/pick_many and describe require compatibility/quota and safe state owners.
- `scripts/model_route_preferences.py#unit-02`: receipt/selection and locked
  spreading-state publication remain for a subsequent coherent persistence pass.
- Both `skills/orchestrate/scripts/exec_routing.py` implementation units:
  batch-1 fallback contracts/parsers remain; execution plans, account reselection
  and scoped cooldown effects still need quota/records integration.
- Batch-1 `scripts/model_route.py` implementation units remain partial; this
  batch adds their free-pattern/privacy dependencies but does not claim resolve,
  catalogue loading, snapshot, or emit are implemented.

## Pending owners and shared dependencies

- `pending.rs`: Python strip, truthiness, numeric equality-to-one, string/repr,
  ordered container repr, Unicode decimal digits and ISO timestamp parsing belong
  to `provenant-core`. This is an ISO reader, not a float-second converter.
- `casefold.rs`: pre-existing Python Unicode casefold/printability tables await
  a shared compatibility API.
- `ordered_json.rs` and the checked ingress in `pending.rs` belong to
  `provenant-records`. Its checked codec exists on the baseline, but internal
  crates are not exposed in `[workspace.dependencies]`. This lane may only add
  dependencies from that table and cannot edit the workspace manifest. Retain
  the private copy until the integration owner exposes the owner dependency;
  then use checked_json while preserving order in the ordered deserializer.

No new behavior change is intended. J1/J2/J3, independent mutant judges,
platform conformance and command cut-over remain with the integration owners.

## Verification and review

- `cargo fmt --all`: passed with the pinned toolchain.
- `cargo clippy -p provenant-routing --all-targets -- -D warnings`: passed.
- `PROVENANT_PORT_DIFF_REQUIRED=1 cargo test -p provenant-routing`: 49 passed,
  zero failed/skipped. Only the targeted routing crate suite ran locally.
- Independent native source review by `codex/gpt-6.1-sol@high`: clean after
  repairs. Findings repaired a relocated fixture path, stale split-module
  imports, missing training/default/refresh policy fixtures, and the typed
  provider request boundary. The fallback oracle now exercises 72 combinations;
  the deadline oracle compares complete probe requests and failure outcomes.
- Main was merged into the lane; no rebase. CI remains the PR landing gate.
