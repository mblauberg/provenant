# Differential divergence log

Version: 1. No whole-project old/new differential run has been performed by
this documentation lane. An empty log is not parity evidence.

For each mismatch, append a stable ID and the fields below. Keep fixed entries
as evidence; supersede them by reference rather than erasing them. Fixtures
must redact secrets and bind only named volatile fields. Link retained run
artefacts and CI evidence at the exact candidate and oracle revisions.

| Field | Required content |
|---|---|
| ID / unit | `RP-<number>`, graph node ID, rulebook version |
| Class | `bug` (Rust defect), `library` (dependency semantics), `python-bug` (old implementation defect, including TS), or `improvement` (intentional new behaviour) |
| Reproducer | Command/MCP transcript, fixture and normaliser revision; platform, config digest and isolated roots |
| Compared revisions | Pinned old revision/binary digest, new head/binary digest and judge revision |
| Expected / actual | Exact differing bytes or schema/value/effect paths, including exit and stderr |
| Evidence | Old-pass/mutant-fail judge proof and differential run artefacts |
| Disposition | Open, fixed (commit and rerun), or approved exception (approver, authority, scope and surviving gate) |
| Follow-up | Regression case and any rulebook/generator amendment |

`bug` and `library` stay blocking until fixed or explicitly approved as a
boundary change. `python-bug` and `improvement` need user approval before the
new behaviour becomes the contract. No lane can approve its own exception.

Existing front-door exceptions D1–D3 are already recorded in
[ADR 0027](../../adr/0027-rust-front-door.md#intentional-divergences-from-the-python-front-door).
They form the current baseline; they are not newly discovered port mismatches.

## RP-1010: telemetry exact numeric counters

- Unit: `scripts/telemetry.py#interface`, `scripts/telemetry_costs.py#unit-01`,
  `scripts/telemetry_report.py#unit-01`; rulebook v2. Class: `bug`.
- Compared revisions: Python and policy tests at
  `e897b10d58b27a46544969594eed08a5ed64c72c`; initial Rust unit commits
  `7b8aabe6`, `541f2f41`, `075f8ed4`; repaired candidate is this PR's head.
- Reproducer/evidence: crate test
  `differential::numeric_boundaries_and_python_float_hash_spelling`, isolated
  in-memory records, pinned Rust toolchain and Python 3.14. No live state.
- Expected/actual: `9007199254740993` must remain exact in retained usage and
  sums. Initial Rust `f64` fields rounded it to `9007199254740992` and could
  collapse adjacent attempt numbers.
- Disposition: fixed with integer/float numeric records, checked exact counter
  sums and exact integer latest-attempt comparisons. Convert to floats at
  pricing/measurement boundaries. The regression rejects the original fields.
- Follow-up: records owner folds `pending::Numeric` into the shared record API;
  full independently owned J2/mutant receipts remain an integration gate.

## RP-1011: telemetry Python float hash spelling

- Unit: `scripts/telemetry_ingest.py#unit-01`; rulebook v2. Class: `bug`.
- Compared revisions: same oracle as RP-1010, initial Rust `3588b22d`, repaired
  candidate is this PR's head.
- Reproducer/evidence: `differential::numeric_boundaries_and_python_float_hash_spelling`
  compares Python SHA-256 over JSON with Rust for `1e-5`, `1e-7`, `1e16`,
  `1e20`, negative zero, Unicode and an exact integer above 2^53.
- Expected/actual: Python hashes `1e-07`; initial Rust hashed `1e-7`.
  A fractional fallback token counter could therefore receive a different ID.
- Disposition: fixed by matching Python's shortest float spelling, exponent
  signs/zero padding and ASCII string escaping. Oracle comparison passes.
- Follow-up: canonical encoder belongs in `provenant-records`; this bounded
  fixture set does not claim exhaustive float-codec/J2 parity.

## RP-1012: telemetry peers accept integral floats

- Unit: `scripts/telemetry_hosts.py#interface`; rulebook v2. Class: `bug`.
- Compared revisions: same oracle as RP-1010, initial Rust `7088692b`, repaired
  candidate is this PR's head.
- Reproducer/evidence: `differential::peer_integral_floats_remain_accepted`.
  Python's peer validator accepts `rows: 1.0` and integral missing-token counts.
- Expected/actual: Rust's validator accepted these numbers but subsequent
  `u64` deserialisation rejected them.
- Disposition: fixed by retaining integer/float numeric count slots through
  typed deserialisation, including integral floats above the u64 range. Regression passes and rejects the old path.
- Follow-up: full peer snapshot/transport conformance is outside this interface
  slice; independently owned J2/mutant receipts remain an integration gate.


## RP-1013: lexical JSON integers beyond u64 remain blocked

- Unit: `scripts/telemetry_hosts.py#interface`, and later JSON/record integration
  for the partial ingestion/report units; rulebook v2. Class: `library`.
- Compared revisions: same Python oracle as RP-1010; Rust candidate is this
  PR's head with default workspace `serde_json` features.
- Reproducer: parse `100000000000000000001` as Python JSON and as
  `serde_json::Value`. Python retains an exact integer; the default Rust Value
  parser converts the lexical integer beyond u64 to an approximate float.
- Expected/actual: peer aggregate counters can exceed one SQLite row's i64
  range. Passing them through Value can silently lose precision before the
  typed numeric visitor sees the input.
- Evidence: independent native repair review identified the Value boundary;
  existing above-2^53 fixtures cover exact i64/u64 records and do not prove
  exact lexical JSON above u64. No independent old-pass/mutant receipt claimed.
- Disposition: **open, blocking affected unit completion and promotion**.
  The hosts interface is partial. No feature expansion, wire narrowing or
  intentional behaviour change is approved. The PR is a draft; no callers or
  live state switch to this crate.
- Follow-up: shared records/integration owner must supply an exact lexical
  number codec or obtain the rulebook/ADR authority for a serde_json feature
  expansion, then add independently owned large-counter conformance fixtures.
### RP-1013 resolution under rulebook v4

- Unit: checked JSON/record boundaries for telemetry and its pending records
  codec; rulebook v4 R4, class `improvement` (supersedes the open v2 disposition
  above). Approved by the rulebook and the PR #1295 follow-up instructions.
- Rulebook revision: `41b1219cc3fb1a3febc0a47a851dc57d165bee78` (PR #1297).
  Python source/policy oracle remains `e897b10d58b27a46544969594eed08a5ed64c72c`;
  compared Rust WIP is `3e17af47`, repaired candidate is this PR's head.
- Expected/actual: Python accepts unbounded lexical integers; Rust now retains
  every i64/u64 integer exactly and refuses larger integers with typed
  `NumberOutOfRange`, before serde_json can approximate them as f64. Arithmetic
  also refuses integer results outside that wire range. No arbitrary-precision
  workspace feature is enabled.
- Reproducer/evidence: `cargo test -p provenant-telemetry -j 2` from
  `tools/front-door`, macOS arm64, Python 3.14.8 and
  `PROVENANT_PORT_DIFF_REQUIRED=1`. Surviving fixtures:
  `pending::decode::tests::checked_decode_retains_spelling_at_integer_boundaries`,
  `ingest::tests::review_regression_oversized_integer_is_a_typed_error` and
  `pending::numeric::tests::arithmetic_refuses_integers_outside_wire_range`.
  The arithmetic regression failed before repair; all 55 crate tests pass
  after repair. Independently owned mutant-fail evidence is not claimed.
- Disposition: resolved by the v4-approved refusal; no longer blocks the typed
  peer interface. Peer transport/merge implementation remains partial.
- Follow-up: move the marked checked codec and numeric contracts from `pending`
  into `provenant-records` at integration; retain boundary/error regressions.

## RP-1001 — host identity JSON input (partial unit)

- Unit: `scripts/host_identity.py#unit-01`; rulebook v2 with the inert-crate
  judge-gate override from rulebook v3 / PR #1294.
- Class: `library`. Source/oracle revision:
  `e897b10d58b27a46544969594eed08a5ed64c72c`.
- Reproducer: in `tools/front-door`, run
  `cargo test -j 2 -p provenant-core python_extended_host_json -- --ignored`.
  Uses isolated temporary hosts files and the repository's Python module.
- Expected/actual: Python accepts UTF-8 BOM, UTF-16/32 byte encodings, nonfinite
  numbers, and lone surrogate escapes in otherwise unused members. The current
  UTF-8/serde reader rejects these and falls back to the hostname slug.
- Evidence: executable Python differential reproducer retained as an ignored,
  failing parity test. Ordinary host policy and duplicate-member fixtures pass.
  No independent old-pass/mutant-fail judge evidence is claimed.
- Disposition: open; this unit is partial and must not cut over. Slugs, name
  validation, strict duplicate detection, fallback, aliases and peer exclusion
  are ported. A compatibility representation/input decoder remains; no
  feature expansion or new dependency is authorised by this entry.

## RP-1002 — JSONC value representation and diagnostics (partial unit)

- Unit: `scripts/lib/jsonc.py#unit-01`; same rulebook and oracle as RP-1001.
- Class: `library`.
- Reproducer: `cargo test -j 2 -p provenant-core python_extended_jsonc_values
  -- --ignored` in `tools/front-door`.
- Expected/actual: Python accepts `NaN`, infinities, arbitrary-size integers
  (subject to its interpreter limit), overflow-to-infinity floats, and lone
  surrogate escapes. The current serde value model rejects or rounds these.
  Generic syntax errors also retain serde diagnostics rather than Python's
  message, character offset, line and column. Unterminated block comments
  already carry Python-compatible character positions.
- Evidence: ignored differential test demonstrates the value acceptance gap;
  active differential fixtures cover comments, escaped strings, duplicate
  members, malformed syntax and trailing commas. Judge evidence is deferred.
- Disposition: open; partial syntax port only. Value representation and edge
  error rendering remain. No changed contract is approved.

## RP-1003 — product pointer JSON/path encoding (partial unit)

- Unit: `scripts/lib/product_root_resolver.py#unit-01`; same rulebook and oracle
  as RP-1001.
- Class: `library`.
- Reproducer: `cargo test -j 2 -p provenant-core python_extended_pointer_json_and_non_utf8_paths
  -- --ignored` in `tools/front-door` on Unix. The fixture creates a product
  directory with byte `0xff` in its name when the filesystem permits it; the
  numeric fixture is independent of filename support.
- Expected/actual: Python writes the path using surrogateescape and JSON ASCII
  escapes, then reads it back. Rust currently returns `PointerError::Encoding`
  for that path and cannot read lone surrogate escapes. Nonfinite/overflow
  numbers or lone surrogates in unused JSON members likewise cause Rust to
  reject a pointer Python accepts.
- Evidence: the numeric differential fails; non-UTF-8 filename creation was
  refused by the local filesystem and that subcase is unverified locally.
  Active byte comparison covers Unicode
  paths. Policy tests cover stale pointers, invalid versions/types, containment,
  linked-directory refusals, linked-file replacement, private file modes and
  concurrent publication. Judge evidence is deferred.
- Disposition: open; pointer loading/writing for ordinary UTF-8 JSON paths is
  ported, including atomic publication. Non-UTF-8 record encoding and the shared
  JSON adaptation remain. The private `pending::atomic_write` belongs in
  `provenant-supervisor` once its safe publication API exists on main.
## RP-1001 - Python refusal string escaping

- Unit: `scripts/lane_safety_guard.py#unit-01`, rulebook version 2.
- Class: `bug` (fixed; no contract change).
- Reproducer: `cargo test -j 2 -p provenant-cli decisions_match_python_source_on_adversarial_argv`
  from `tools/front-door`, Python 3.14.8 / Unicode 16.0.0, macOS arm64.
  The fixture passes a single U+200B target, with no filesystem or volatile-field
  normalisation and no provider/config access.
- Compared revisions: Python source and policy oracle
  `5c48d4e5567c4801b072bbf88203e88822079ab1`; faulty Rust policy
  `b2ed1154`; fixed implementation and differential fixture
  `d789ac2cef086e955f1ac1ba8fe23c440dc05dbf`.
- Expected: `kill target '\u200b' is not a literal PID`, with ASCII escape
  bytes. Actual: the target contained literal UTF-8 `e2 80 8b` instead.
- Evidence: the retained differential test failed on this exact comparison
  before repair and passes after repair. `repr_and_strip_match_python_on_control_and_unicode_boundaries`
  adds 377 character/string fixtures, including controls and supplementary
  Unicode. The generated printable-property table pins the oracle's Unicode
  version instead of relying on Rust's differing character properties.
- Disposition: fixed in `d789ac2c`. Independent J1 acceptance remains pending;
  these lane differential fixtures do not replace the independent judge.
- Follow-up: move the private compatibility primitives to `provenant-core`
  during integration; retain the differential cases. No rule amendment or
  intentional exception is requested.

## RP-1002 - Cleanup request error classification

- Unit: `scripts/clean.py#interface`, rulebook version 2.
- Class: `bug` (fixed; no contract change).
- Reproducer: `cargo test -j 2 -p provenant-cli invalid_request_error_classes_match_python_streams_and_exit_codes`
  from `tools/front-door`, Python 3.14.8, macOS arm64. Fixtures use only
  invalid arguments, before any plan, Git query or deletion is attempted.
- Compared revisions: Python oracle
  `5c48d4e5567c4801b072bbf88203e88822079ab1`; faulty renderer
  `d789ac2c`; repair and differential test
  `a79987fea684086aa696b4400e3ed265d713d83c`.
- Expected / actual: approval-pair, prune-options and unexpected-selector
  failures use parser usage plus `clean.py: error: ...`, exit 2. Rust's generic
  renderer instead emitted `provenant clean: ...`. Missing-selector text was
  already correct and remains the special case without usage.
- Evidence: native source-parity review identified the classification error;
  four source differential cases now compare stdout, stderr and exit codes.
  Usage is an explicit parser-owner input; usage/help generation is still
  listed as partial, not claimed as ported by this test.
- Disposition: fixed in `a79987fe`; native repair review is clean. Independent
  J1 acceptance remains pending. No intentional exception is requested.

## RP-130401 - Routing wire shapes, order and numeric precision

- Units: `skills/orchestrate/scripts/exec_routing.py#interface`,
  `scripts/model_route_catalog.py#unit-01`, and partial pools/preferences/live
  units; rulebook version 4 (PR #1297).
- Class: `bug` (fixed by the PR #1304 repair; no live cut-over).
- Compared revisions: Python source at `6aa0097083aa0e9f2f69d2d60fe9f8023dcae594`,
  Rust before repair at `914996a4`; repaired Rust and judges are in PR #1304.
- Reproducer: `cargo test -p provenant-routing -j 2` in `tools/front-door`,
  macOS arm64, using the configured Python >=3.12 with `-I`. No path-valued
  fixtures or live providers are involved.
- Expected / actual: fallback policy null was `"default"`; lists were tagged
  and normalized; parsed candidates dropped null effort and dictionary order;
  execution plan fields did not match `candidates()` inputs; family scans used
  unordered maps; empty models read missing patterns; cache snapshots used
  caller-supplied age rather than `observed_at`.
- Evidence: null-policy, empty-pattern and integer-weight regressions failed
  before repair. Exact wire round trips, source differentials, ordered scans,
  integer boundaries and the 86400-second freshness boundary survive as tests.
  Native repair review also caught integer `-0` becoming float `-0.0` and
  range errors in JSON-string policies losing their type; both now have
  witnessed failing regressions and repairs.
- Disposition: fixed; J1/J2/J3 remain cut-over gates. Compatibility primitives
  are private pending copies, marked with their future core/records owner.
- Follow-up: integrate checked integer JSON ingress and Python compatibility
  helpers into the owning crates. `pending::from_json` rejects integers outside
  i64/u64 with typed `NumberOutOfRange`, as required by v4; callers must use it
  rather than an unchecked `serde_json` decoder at JSON ingress.

## RP-130402 - Lazy alias re-expansion across adapters sharing a family

- Unit: `scripts/model_route_catalog.py#unit-01`, rulebook version 4.
- Class: `bug` (open Rust mismatch; catalog unit remains partial).
- Compared revisions: Python source at `6aa0097083aa0e9f2f69d2d60fe9f8023dcae594`,
  Rust in PR #1304.
- Reproducer: family aliases `{"senior":["sonnet"]}` with adapters `a` and `b`
  both declaring that fixed family. Python's lazy generator snapshots the
  expanding candidate set once per adapter, in set iteration order.
- Expected / actual: Python can include `b-a-sonnet` (or `a-b-sonnet`), whereas
  Rust expands only the original names and returns `sonnet`, `a-sonnet`,
  `b-sonnet`. This can change override reservation validation.
- Evidence: source comparison of `family_alias_candidates`; no parity claim
  is made for this multi-adapter case.
- Disposition: open, recorded at the repair request's direction. Matching
  Python's set-order-dependent expansion needs a dedicated policy decision;
  no exception or live cut-over is authorised here.
- Follow-up: retain this counterexample for the remaining catalogue unit work.

## RP-130403 - Exact pool weight numbers under v4

- Unit: partial `scripts/model_route_pools.py#unit-01`, rulebook version 4.
- Class: `improvement` (explicitly requested in PR #1304's repair instructions).
- Compared revisions: Python source at `6aa0097083aa0e9f2f69d2d60fe9f8023dcae594`,
  Rust repair in PR #1304.
- Reproducer: `integer_weights_retain_exact_integer_wire_form`; input integer
  `9007199254740993` formerly became `9007199254740992.0`.
- Expected / actual: v4 preserves integer inputs and named integer weights as
  integers; float inputs remain floats. Python's `weight_value` explicitly
  coerces weights to floats, so exact integer preservation deliberately follows
  the repair request and R4 rather than that coercion.
- Disposition: implemented within the user's explicit numeric repair scope;
  out-of-range integers are refused rather than rounded. No provider routing
  is switched to Rust by this inert crate.
## RP-1014: telemetry Python value and Unicode semantics

- Units: `scripts/telemetry_changes.py#unit-01`,
  `scripts/telemetry_costs.py#unit-01`, `scripts/telemetry_report.py#unit-01`;
  rulebook v4, class `bug` (fixed; no contract change).
- Compared revisions: Python source/policy oracle
  `e897b10d58b27a46544969594eed08a5ed64c72c`; faulty WIP `3e17af47`,
  repaired candidate is this PR's head. Canonical Unicode oracle is CPython
  3.14.8 / Unicode 16.0.0, matching main's copied CLI compatibility tables.
- Expected/actual: Python strips U+001C–U+001F, accepts Unicode decimal window
  and model suffix digits, and rejects U+0345 as an alphabetic alias. The old
  Rust paths differed. Python merges equal numeric keys `1`/`1.0` and sorts
  numeric dimensions using Python string spellings; Rust separated them and
  mapped numeric sort keys to an empty string. Active-time integer
  `9007199254740993` became `9007199254740992` through an f64 accumulator.
- Reproducer/evidence: the `unicode_windows_and_refusals_match_python`,
  `unicode_model_attribution_matches_python`,
  `numeric_group_keys_and_order_match_python`,
  `table_trailing_whitespace_matches_python` differential fixtures and
  `changes::tests::active_time_preserves_large_integer_values` all failed
  before their repairs and pass afterwards. Letter classification also
  matches CPython over all Unicode scalar values. Run
  `PROVENANT_PORT_DIFF_REQUIRED=1 cargo test -p provenant-telemetry -j 2`
  in `tools/front-door` on macOS arm64; 55 tests pass.
- Disposition: fixed with copied/extended compatibility primitives in `pending`
  and exact numeric accumulation/comparison. Independent native source review
  identified the model, cost-key and table defects. Independently owned
  old-pass/mutant-fail judge receipts are not claimed.
- Follow-up: integrate the marked helpers into `provenant-core`; ledger query
  adapters remain with the store owner and implementation units stay partial.
## RP-FR1-001: generic gate-extension JSON numbers (open, library)

- Unit: `runtime/fabric/src/graph/gates.ts#unit-01` (partial), rulebook v2.
- Compared source/oracle: `e897b10d58b27a46544969594eed08a5ed64c72c`;
  candidate: the `port/fabric-runtime-1` batch, `gates::parse_gate_exit`.
- Reproducer: an exit record with a matching token, string `finished_at` and
  an extension field `"n":1e400`. Node `JSON.parse` accepts the number as
  Infinity; generic `serde_json::Value` rejects it. Large finite integers also
  have different JavaScript rounding semantics. This is not an accepted change.
- Expected/actual: TS returns the record; the current partial Rust decoder can
  return no record for an out-of-range number. Normal typed gate exit codes do
  not exercise this extension-field case.
- Evidence: the parser probe recorded in the batch's local verification log;
  no independent mutant judge is claimed for this remaining boundary.
- Disposition: open and blocking completion/cut-over of the gate unit. Preserve
  the partial designation. The integration owner must settle the response codec
  and add the boundary judge before claiming this unit fully ported.
