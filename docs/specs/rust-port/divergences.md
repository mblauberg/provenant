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

## RP-config-1 — workspace source identity

- Unit: `scripts/lib/front_door_build.py#unit-01`; rulebook version 2.
- Class: `improvement`; required by R2 and ADR 0028's accepted workspace
  digest expansion. The new library is inert; the active builder/checker are
  unchanged and must switch together in the integration pass.
- Compared revisions: Python oracle `5c48d4e5`; Rust branch `port/config-1`;
  judge is `front_door::tests::digest_covers_workspace_libraries_and_build_inputs`.
- Reproducer: edit a member's `src/lib.rs`, `Cargo.toml`, or `build.rs`, or
  root `build.rs` / `.cargo/config.toml`, then recompute the source identity.
- Expected / actual: Python's root-only digest stays unchanged; Rust's digest
  changes. Existing framing and root-only fixtures match Python exactly.
- Evidence: the targeted digest test changes and restores each named input;
  `python_digest_differential_without_workspace_members` compares old/new.
- Disposition: required design change, prepared only. Activation awaits the
  workspace builder/checker integration and its stale-library gate.
- Follow-up: carry the identical input set to the active builder and checker
  before linking a library into the installed front door. Build, smoke and
  select execution remain partial until supervisor's safe runner exists.

## RP-config-2 — record codec integration remains partial

- Units: `scripts/install-workflows#unit-01`,
  `scripts/managed_installation_manifest.py#unit-01`; rulebook version 2.
- Class: `library`; unresolved, blocks claiming either unit fully ported or
  activating the new persistence APIs. This is not an approved behaviour change.
- Compared revisions: Python oracle `5c48d4e5`; Rust branch `port/config-1`;
  fixtures live in `pending::tests` in the config crate.
- Reproducer: load an opaque receipt field containing integer
  `18446744073709551616`, a non-finite number, or a lone surrogate; or construct
  a receipt for a Unix path containing non-UTF-8 bytes.
- Expected / actual: Python preserves arbitrary integers, non-finite JSON
  values and surrogate-escaped filesystem strings; the workspace's default
  serde_json model cannot retain all of them. Rust explicitly refuses
  out-of-range integer input and non-UTF-8 path conversion instead of silently
  rounding or replacing bytes. Non-finite/lone-surrogate JSON fails parsing.
- Evidence: `lossless_integer_boundary_is_explicit` and
  `non_utf8_paths_never_silently_change_the_receipt` pin the integration gap;
  ordinary receipts, opaque history, Unicode, float thresholds, ASCII escaping
  and sorted canonical JSON have Python differential tests.
- Disposition: open; affected units are partial. No dependency feature expansion
  is requested or applied by this lane.
- Follow-up: records owner supplies a lossless wire codec under the allowlist,
  and J2 supplies the currently absent installation schemas. The chair owns
  that boundary decision; no command cut-over is included here.

## RP-config-3 — Codex TOML parser diagnostic wording

- Unit: `scripts/configure-codex-skill-overrides.py#unit-01`; rulebook version 3.
- Class: `library`; Python uses `tomllib`, Rust uses `toml_edit`.
- Compared revisions: Python oracle `5c48d4e5`; Rust branch `port/config-1`
  repair of PR #1290. Regression:
  `codex::tests::invalid_toml_keeps_python_prefix_and_typed_source`.
- Reproducer: call Python `state("[skills")` and Rust `codex::state("[skills")`.
- Expected / actual: both reject malformed TOML and carry the stable
  `Codex config is invalid TOML: ` prefix. Parser explanation, source excerpt
  and location formatting after that prefix may differ. Rust preserves the
  typed `toml_edit::TomlError` as its error source.
- Evidence: the regression asserts the prefix and typed variant; the compared
  parser diagnostics are reproducible with the fixture above. This is library
  evidence, not an old/new CLI judge run.
- Disposition: open wording mismatch, recorded for CLI integration; no approved
  boundary exception or cut-over is claimed. The unit remains partial.
- Follow-up: reconcile or obtain approval for parser wording at the CLI edge;
  implement Python's exit-3 `conflicting:` stderr wrapper there and run the
  cut-over judge. The wrapper is not part of this inert library repair.
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

## RP-records-001 — Python adapter JSON numeric and whitespace semantics

- Unit: `skills/orchestrate/scripts/attempt_evidence.py#unit-01`; rulebook v3.
- Class: `library` (numeric parsing) and `bug` (blank-line classification).
- Reproducer: crate test `differential_python_success_matrix`, cases
  `nonfinite_unknown`, `overflow_unknown`, `negative_zero_adapter_exit`,
  `python_blank_tail`, plus `differential_python_record_numeric_spelling`; macOS arm64,
  isolated temporary run directory, no environment/config normalisation.
- Compared revisions: Python oracle `9797088841644372af432f64b6dd3918bbdd5c19`;
  Rust before repair `3f471de7`; J2 baseline `d9fcebd4`. No independent mutant
  proof is claimed for these added differential cases.
- Expected / actual: Python accepts ignored `NaN`, infinities and `1e400`
  receipt fields; strict serde parsing refused them as invalid JSON. Python
  parses JSON `-0` as integer zero while serde retains floating negative zero. Python
  ignores a trailing U+001F-only line; Rust initially treated it as JSON.
- Disposition: fixed in this records batch. The validation projection maps
  nonfinite/beyond-range floats to null (none can satisfy a numeric success
  field). V4 integer overflow is now the typed refusal in RP-records-005.
  The projection preserves integer `-0` and Python blank-line rules without
  changing retained bytes.
  Differential rerun passes. No intentional boundary change.
- Follow-up: retain these regression cases; independently judge them before
  cut-over. No rulebook/generator change required.

## RP-records-002 — Non-strict symlink resolution

- Unit: `skills/orchestrate/scripts/attempt_evidence.py#unit-01`; rulebook v3.
- Class: `bug`.
- Reproducer: crate test `differential_python_non_strict_symlink_resolution`,
  self-loop and a 50-link acyclic chain; macOS arm64, isolated tmp directory.
- Compared revisions: Python oracle `9797088841644372af432f64b6dd3918bbdd5c19`
  under Python 3.14.8; Rust before repair `3f471de7`; J2 baseline `d9fcebd4`.
- Expected / actual: non-strict Python preserves an unresolved loop and resolves
  an acyclic chain longer than 40 links; Rust refused both after 40 traversals.
- Evidence: independent native review, followed by the differential case; no
  independent mutant proof claimed.
- Disposition: fixed in this batch by tracking unresolved/resolved link paths
  rather than a global traversal limit. Python <3.13 skips only the loop
  differential because its non-strict loop semantics differ from the oracle.
- Follow-up: move the pending resolver to core and retain the regression.

## RP-records-003 — Parent traversal escapes removal anchor

- Unit: `skills/orchestrate/scripts/attempt_storage.py#unit-01`; rulebook v3.
- Class: `python-bug` (proposed correction; not an accepted boundary change).
- Reproducer: `remove_tree(anchor, anchor / '..' / 'victim')` on one device,
  with a sibling victim containing a fixture file. Python traverses `..` and
  deletes the sibling. The Rust test
  `removal_refuses_parent_components_before_opening` models that descriptor
  traversal and proves that the proposed refusal preserves the sibling.
- Compared revisions: Python oracle `9797088841644372af432f64b6dd3918bbdd5c19`;
  Rust before repair `3f471de7`; native source-parity review identified it.
- Expected / actual: the documented containment promise requires refusal;
  Python's lexical relative path permits traversal outside the anchor.
- Evidence: native review and isolated fixture reproduction. No live owner or
  user data was involved; no independent mutant proof claimed.
- Disposition: open. Inert Rust refuses non-normal relative path components;
  no caller is switched. Storage is reported partial, with this proposed
  correction awaiting explicit boundary authority before cut-over.
- Follow-up: settle the compatibility decision, integrate supervisor's safe OS
  binding, and run descriptor-based adversarial conformance on both platforms.

## RP-records-004 — Decode error causes

- Unit: `skills/orchestrate/scripts/attempt_evidence.py#unit-01`; rulebook v3.
- Class: `bug`.
- Reproducer: `decode_failures_retain_typed_causes`, invalid UTF-8 and malformed
  JSON adapter receipts.
- Compared revisions: Python oracle `9797088841644372af432f64b6dd3918bbdd5c19`;
  Rust before repair `45c46d22`; native repair review identified the gap.
- Expected / actual: Python chains the decode exception; Rust initially kept
  only the stable policy message, losing the underlying failure category.
- Evidence: native review and crate source-chain assertions; no independent
  mutant proof claimed.
- Disposition: fixed in this batch with typed UTF-8/serde causes and unchanged
  policy display text. The raw-record API also retains its decode causes.
- Follow-up: preserve source-chain coverage at the future record-reader edge.

## RP-records-005 — Checked integer range (rulebook v4)

- Unit: records shared codec and `attempt_evidence.py#unit-01`; rulebook v4.
- Class: `improvement` (required by R4, inert until cut-over).
- Reproducer: `checked_json::tests::out_of_range_integers_are_typed_even_in_unowned_fields`
  and `raw_validation_refuses_out_of_range_integers_with_typed_cause`.
- Compared revisions: Python oracle `9797088841644372af432f64b6dd3918bbdd5c19`;
  Rust before repair `101aadbe`; macOS arm64, Python 3.14.8.
- Expected / actual: Python accepts arbitrary-size integers; serde can round
  integers beyond u64 into f64, including ignored fields. The exported records
  codec checks all tokens and returns typed `NumberOutOfRange` outside i64/u64.
  In-range integers retain integer spelling, including cooldown backoff `30`;
  float tokens retain f64. No epsilon or arbitrary_precision feature is used.
- Evidence: the raw-validation regression accepted an ignored oversized
  integer before the repair; cooldown round-trip emitted `30.0` before repair.
- Disposition: follows accepted R4 improvement; other crates' pending checked
  codecs can be removed when integrated with this owner. No command cut-over.

## RP-records-006 — Lone-surrogate JSON escapes

- Unit: `skills/orchestrate/scripts/attempt_evidence.py#unit-01`; rulebook v4.
- Class: `library` (open before cut-over).
- Reproducer: `lone_surrogate_unknown_fields_are_typed_json_refusals`, escaped
  `\ud800` and `\udfff` in an ignored record field; macOS arm64, Python 3.14.8.
- Compared revisions: Python oracle `9797088841644372af432f64b6dd3918bbdd5c19`;
  Rust before repair `101aadbe`.
- Expected / actual: CPython json.loads accepts unpaired surrogate code points,
  including in ignored fields that otherwise validate. serde_json/String cannot
  represent these Unicode non-scalars and validation refuses with
  `RecordJson(JsonCause::Json(_))`; adapter JSON has the corresponding typed cause.
- Evidence: test checks CPython acceptance and typed Rust refusal. Retained
  bytes remain opaque; nothing rewrites them.
- Disposition: recorded library mismatch, not approved parity. Resolve the
  representation/boundary before cut-over; this inert PR does not switch callers.
## RP-1301 — supervisor partial numeric decoders

- **Unit / rules:** `skills/orchestrate/scripts/bin/ps#unit-01` and
  `skills/orchestrate/scripts/process_info.py#unit-01`; rulebook v2.
- **Class:** `library` (Rust integer representation versus Python integers).
- **Reproducer:** Python's ps `parse(['-p', '9223372036854775808'])` retains
  the integer; Rust `ps::parse` returns `PsParseError::PidRange`. Python also
  accepts Unicode decimal PID digits, while this partial Rust parser accepts
  ASCII digits. Python `_duration(1e20)` formats a finite value; the partial
  Rust decoder returns `ProcessParseError::Duration` at/above `i64::MAX`.
- **Compared revisions:** Python/TS source and original tests at
  `5c48d4e5567c4801b072bbf88203e88822079ab1`; Rust slice introduced by
  `73d532b2` and `9763fa64`. Source units remain unchanged in integrated main
  `e897b10d58b27a46544969594eed08a5ed64c72c`. No command/binary comparison is
  claimed: these APIs have no command callers. There is no normaliser.
- **Expected / actual:** See reproducer; this is an incomplete domain, not an
  accepted refusal at the command boundary. No effect or output is switched.
- **Evidence:** Existing Python policy tests and inline differential fixtures
  cover the implemented domain. Independent old-pass/mutant-fail numeric-domain
  evidence remains with the judge owner; none is claimed by this slice.
- **Disposition:** Open; blocks full acceptance of both affected units. No
  intentional behavior change or exception is requested or approved.
- **Follow-up:** Extend integer/Unicode decoding and duration representation,
  and include these cases in independent J1 fixtures before cut-over. Scope
  and other remaining portions are in the supervisor crate's README.

### RP-1301 resolution

The supervisor entry was numbered RP-1001 on its unlanded branch; merge with
main assigned RP-1301 to avoid the already landed refusal-escaping entry.
Fixed by `a70c4d82` (finite float durations, NaN/negative-infinity clamp) and
`037176dc` (large PID integers and Unicode decimal digits). The numeric
fixtures now compare the former counterexamples against Python 3.14.8,
including `f64::MAX`, without normalising any result. The open numeric-domain
limitation above is superseded by this repair. Independent judge coverage
remains a cut-over requirement under rulebook v3.

## RP-1302 - Bounded supervisor ps fallback and typed failures

- **Unit / rules:** `skills/orchestrate/scripts/process_info.py#unit-02`;
  rulebook v3. The dependent ps port retains this partial error domain.
- **Class:** `library`; incomplete refusal/exception compatibility, not an
  accepted command change.
- **Reproducer:** The Python `_ps_processes` subprocess capture is unbounded.
  The inert Rust observation adapter refuses stdout or stderr over 1 MiB,
  and returns typed timeout/I/O/decode errors. In particular, its timeout
  display is currently `ps census failed: process timed out`, rather than
  Python's `TimeoutExpired` command/timeout detail. No normaliser is used.
- **Compared revisions:** Unchanged Python source at
  `e897b10d58b27a46544969594eed08a5ed64c72c`, integrated main
  `50972eb5c1d8e208a28c0ef6d11ab0dd5e522814`; adapter introduced in
  `6f150b1c`. No command imports the Rust crate, so no binary equivalence
  or external refusal change is claimed.
- **Expected / actual:** Outputs within the implemented domain are decoded
  strictly with universal newline handling. Oversize output and timeout
  errors differ as above; generic serde process text also refuses surrogate
  bytes rather than inventing a UTF-8 value. Native OS/ps text preserves those
  bytes separately up to its explicit record edge.
- **Evidence:** Translated Rust policy tests, pure fixture differentials and
  native identity comparisons cover the implemented path. The local sandbox
  denies setuid system ps execution, so full fallback conformance is deferred.
- **Disposition:** Open for the affected observation/error boundary; full
  unit-02 parity is not claimed. No improvement or Python-bug exception has
  been approved. Other inert units proceed under R10.3/R11 v3.
- **Follow-up:** Agree the bounded refusal and complete edge error/OS-text
  encoding with the CLI/record owners before cut-over; add independent J1/J2
  failure cases and unrestricted Linux/macOS subprocess conformance.

### RP-1301 v4 integer bounds

Under rulebook v4 R4, PID selection is bounded to i64/u64. For example Python
accepts `18446744073709551616`; Rust now refuses it with typed
`PsParseError::NumberOutOfRange`, rather than representing an arbitrary Python
integer. At the i32 OS PID/record boundary, `2147483648` is likewise a typed
`NumberOutOfRange` (never rounded through f64 or silently dropped by the
text decoder). Darwin task tick addition refuses u64 overflow with
`ProcessParseError::NumberOutOfRange` before converting the duration to float.
This is the rulebook's recorded **improvement**, not command cut-over authority.
Rust Unicode/range and integer-addition fixtures cover these cases. Lookup
and census APIs propagate the typed range errors rather than dropping rows;
the filesystem fixture checks both ppid and pgid overflow and disappearance.
The source revision above is unchanged; CLI rendering remains an edge task.
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
