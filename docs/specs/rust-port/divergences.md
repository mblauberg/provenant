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
