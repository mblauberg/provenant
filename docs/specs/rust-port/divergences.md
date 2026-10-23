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

## RP-1: quota timestamp library semantics (fixed)

- Unit: `scripts/quota_schedule.py#unit-01`; PORTING.md version 2.
- Class: `library`. Python `datetime.fromisoformat` and Jiff's Temporal parser
  have different accepted grammars and precision. Direct Jiff parsing accepted
  leap seconds and bracketed zone annotations, rejected ISO week dates, and
  retained nanoseconds that Python truncates to microseconds. Before the epoch,
  using a timestamp's signed fractional component also formatted incorrectly.
- Oracle: source revision `e897b10d58b27a46544969594eed08a5ed64c72c`, Python
  3.14.3. `scripts/quota_schedule.py` SHA-256:
  `07fe410e1543c5d40ecd7ebfa6d685925c61a922a762bd94e54ed1c720011058`.
  Candidate and judge: the quota crate and its tests in this PR.
- Reproducer/evidence: `iso_time_formats_are_python_compatible` and
  `time_boundaries_match_python_and_overflow_is_unset` run isolated source
  calls against explicit clocks. For example, `2026-W43-5T06:00:00` must become
  `2026-10-23T06:00:00Z`; `2026-10-23T06:00:60Z` must be unset. The week-date
  counterexample failed before repair. Fractional hours and minutes are refused
  by the pinned oracle, including in offsets.
- Disposition: fixed by validating Python's input grammar before Jiff calendar
  arithmetic and constructing microsecond-resolution values. No intentional
  behaviour change. Existing scheduling and report comparisons remain in the
  crate's differential suite; exact-head CI remains the landing gate.

## RP-2: quota optional wire blocks (fixed)

- Unit: `scripts/provider_quota.py#interface`; PORTING.md version 2.
- Class: `bug`. A plain `Vec<UsageCredit>` rejected the explicit JSON null
  accepted by `schemas/quota-snapshot.schema.json` and Python. Plain optional
  fields also collapsed absent and explicit-null blocks, and a nullable budget
  currency could disappear during deserialisation.
- Oracle: source revision `e897b10d58b27a46544969594eed08a5ed64c72c` and that
  revision's independently owned J2 quota snapshot schema. Candidate and judge:
  the quota crate and tests in this PR.
- Reproducer/evidence: `snapshot_wire_nulls_and_j2_contract` round-trips absent
  and null credits, resets and allowance, plus a budget's explicit-null
  currency. It validates the fixture against the existing J2 schema and checks
  that a string-valued `used_percent` mutant is rejected. Python `jsonschema`
  is optional for local differential runs; CI supplies the declared test tools.
- Disposition: fixed with typed absent/null/value blocks and explicit currency
  presence. No intentional behaviour change; record ownership remains marked
  `pending` until the records crate integration pass.

## RP-3: quota date signs and activity precision (fixed)

- Unit: `scripts/quota_schedule.py#unit-01`; PORTING.md version 3.
- Class: `bug`. Signed month/day digits were accepted by integer parsing.
  Activity seconds were rounded after scaling the entire epoch rather than
  splitting the fraction as CPython `datetime.fromtimestamp` does.
- Oracle: Python source revision `e897b10d58b27a46544969594eed08a5ed64c72c`.
- Evidence: `review_regression_signed_date_components_are_rejected` and
  `review_regression_activity_uses_fromtimestamp_fraction_rounding` both failed
  on the pre-fix tree. The latter covers 1735689600791625390 ns converted to
  float seconds, requiring `2025-01-01T00:00:00.791625Z`, and an active-window
  boundary that detects a one-microsecond error.
- Disposition: fixed with ASCII date digits and shared fractional half-even
  timestamp conversion. Duration addition retains Python timedelta rounding.

## RP-4: dangling project links and explicit home expansion

- Unit: `scripts/quota_schedule.py#unit-01`; PORTING.md version 3.
- Class: `bug` for dangling links, `improvement` for explicit home expansion.
- Oracle: Python source revision `e897b10d58b27a46544969594eed08a5ed64c72c`.
- Evidence: `review_regression_dangling_project_symlink_keeps_real_activity`
  failed before repair: a dead project link suppressed a real session file.
- Disposition: dangling links are fixed by testing directory status before
  reading metadata, matching Python's skip. The existing explicit `home`
  argument expands `~` and `~/`; Python instead consults the process home and
  supports `~user`. This dependency injection remains an intentional inert
  library divergence and must be reconciled by the command cut-over owner.

## RP-5: quota integer spelling and tolerant readers (fixed)

- Units: `scripts/provider_quota.py#interface`,
  `scripts/quota_schedule.py#unit-01`, `scripts/quota_display.py#unit-01`;
  PORTING.md version 3.
- Class: `bug`. Floating point wire fields changed `50` to `50.0` and could
  round integers above 2^53. Schedule TTLs must be integers; copied durations,
  amounts, percentages and configuration retain their input number category.
- Oracle: Python source revision `e897b10d58b27a46544969594eed08a5ed64c72c`.
- Evidence: `review_regression_integer_wire_spelling_is_preserved` failed
  before repair. Record comparisons now compare numeric spellings exactly.
  `checked_numbers_preserve_large_integers_and_reject_out_of_range_tokens`
  covers i64/u64 boundaries, 2^53+1, float spelling and oversized nested tokens.
- Disposition: fixed with an int-or-float model and a checked `decode_record`
  boundary in `pending`, marked for the records owner. Integers outside the
  union of i64/u64 return typed `NumberOutOfRange`, including unknown fields;
  they are never rounded through f64. Future ingress owners must use this
  decoder. Python integers beyond that range remain a recorded refusal at
  this inert Rust boundary under the owner's number policy.
- Reader compatibility: unreadable `used_percent` remains intact and is
  skipped by scheduling; null failures become zero in the schedule; an empty
  credits dictionary loads and falls back as Python does. These tolerant
  reader cases do not make malformed producer output schema-valid. The
  existing independent J2 schema still rejects string usage percentages.

## RP-6: differential interpreter and numeric comparison gates (fixed)

- Units: quota crate differential fixtures; PORTING.md version 3.
- Class: `bug`. A present but unsupported Python interpreter failed at import,
  while a missing interpreter silently passed the differential cases. A
  hand-written pool stub also accepted an empty prefix unlike the source.
- Disposition: require a version-checked Python 3.11+ interpreter. With
  `PROVENANT_PORT_DIFF_REQUIRED=1`, missing or unsupported interpreters fail;
  otherwise they print a skip reason. `PROVENANT_PORT_PYTHON` selects an
  explicit interpreter. Pool calls use the real Python provider hook.
- Evidence: `interpreter_version_and_required_differential_contract` checks
  version selection and both missing-interpreter subprocess outcomes;
  `provider_pool_oracle_uses_the_real_hook` covers `.x`.
  `exact_record_comparison_rejects_integer_float_mutants` ensures `50` and
  `50.0` cannot compare equal. No epsilon or f64 comparison normalises records.

## RP-7: quota display edge cases and missing headline binding

- Units: `scripts/quota_display.py#unit-01` and `#unit-02`;
  PORTING.md version 3. Oracle source revision:
  `e897b10d58b27a46544969594eed08a5ed64c72c`.
- Class: `bug` for empty resets, numeric history and duplicate names. Empty
  reset cells now print `-`; signed-out numeric observation times retain
  their spelling; summaries use the selected row's index after enrichment.
  Window labels distinguish Python regex decimal digits and `isdigit` from
  broader numeric characters such as fractions and Roman numerals.
- Evidence: the three `display_review_regression_*` tests failed on the
  pre-fix tree for duplicate-name summary selection, empty reset rendering
  and a fraction treated as a digit. Fixtures also cover superscript and
  Arabic digits and numeric signed-out history against the Python oracle.
- Class: `python-bug` for worst-mode headlines with no binding. Python indexes
  `None` and raises `TypeError`; Rust skips that candidate and returns no
  headline if none remain. The owner explicitly requested recording this
  divergence in the PR #1293 repair brief. The regression
  `worst_headline_without_binding_is_an_explicit_python_bug_divergence`
  verifies both outcomes. This exception affects the inert library only;
  relevant independent judges still gate command cut-over under rulebook v3.

### RP-5 follow-up: exact integer calculations and selection (fixed)

- Independent repair review found that retaining integer fields alone still
  allowed TTL arithmetic and interval/window comparisons to round through
  f64. `schedule_keeps_large_integral_ttls_and_exact_interval_order` and
  `display_compares_and_labels_large_integer_windows_exactly` both failed
  before the follow-up repair on `9007199254740993`.
- Integer multiplication, min/max, ordering, rounding and length labels now
  retain exact integers. Mixed integer/float comparisons preserve integer
  precision, and overflow remains typed. Configuration `Value` layers are
  checked before deserialisation, including unknown nested numbers.
- Python's explicit floating point formats (`:g` and `:,.0f`) still use float
  calculation at the text boundary, as the source does. Wire integers do not.
- The follow-up regression `schedule_caps_products_before_integer_or_float_overflow`
  also failed before repair. Scheduling compares positive integer products in
  u128 and applies the configured cap before requiring the retained TTL to fit
  i64/u64. A Python float product that becomes infinity is likewise capped.

### RP-5 / RP-6 follow-up: rulebook v4 boundary alignment

- Rulebook version 4, R4/R9, from `origin/docs/porting-v4`. The prior
  `arbitrary_precision` feature unified across the workspace and broke enum
  decoding. Removed it; `pending::decode_record` checks raw integer tokens
  before serde can turn an oversized integer into a float. Quoted numbers
  and float tokens remain distinct; integer `-0` becomes `0` as in CPython.
  Already-decoded `Value` inputs cannot recover their original token category;
  callers must use the checked decoder at ingress.
- Class: `improvement` for the deliberate i64/u64 refusal, as R4 requires.
  Out-of-range integer tokens, including unknown fields, return typed
  `NumberOutOfRange`. This does not enable the records crate's future codec.
- Oracle selection now uses only `PROVENANT_PORT_PYTHON` (default `python3`,
  >=3.12) or `PROVENANT_PORT_NODE` (default `node`, >=24). Missing/old
  interpreters print `skip:` and fail with `PROVENANT_PORT_DIFF_REQUIRED=1`.
  Temporary roots are canonicalised on macOS; record comparisons retain exact
  numeric spelling, including config tests.
- The sole float-second `from_timestamp` conversion now lives in `pending`,
  marked for `provenant-core`. It splits integral/fractional seconds and
  rounds half-even to microseconds, and refuses dates outside years 1..9999.
  Duration addition retains Python's separate timedelta semantics.
  Decimal-digit, digit and strip helpers are copied from main's CLI pending
  module; whitespace extension remains beside those shared helpers.
- Evidence: the 3.11-floor test and oversized-token test failed before the
  repairs. New codec and fromtimestamp cases failed on integer `-0` becoming
  `-0.0` and Jiff accepting year zero; both now pass against CPython.

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

### RP-7 follow-up: Python string conversion in signed-out history (fixed)

- The native v4 repair review found that `observed_at: 1e-7` rendered as
  `1e-7` instead of Python's `1e-07`; truthy booleans were also displayed as
  `never` instead of `True`. The public report regression reproduced the
  exponent mismatch before repair and now covers exponent floats, booleans
  and container values tolerated by Python's reader.
- Signed-out summaries now use Python truthiness and string conversion in
  `pending`, marked for `provenant-core`. String repr and its Unicode table
  are copied from main's CLI implementation, then extended for JSON values.
  Number conversion keeps integer digits exact and normalises float notation
  from serde's shortest digits, including CPython's exponent padding.
- An exact CPython differential covers notation boundaries, nested strings,
  empty values and 512 deterministic float bit patterns. It also exposed a
  decimal tie where Rust LowerExp selected `...851.13` and CPython selected
  `...851.12`; retaining serde's shortest digits fixes that case.
- Repair re-review also found sorted JSON maps losing Python dict insertion
  order in this same tolerant observation field. `PythonValue` now retains
  its Python representation while decoding, including nested objects and
  duplicate-key replacement in the original position. Its wire serialization
  stays a JSON value. The raw-input public report regression failed before
  repair and now covers reordered, nested and duplicate-key objects without
  adding `preserve_order` or a dependency.
