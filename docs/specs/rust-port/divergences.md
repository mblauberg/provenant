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

## RP-1003: workflow Python JSON representation gaps

- Units: `skills/_shared/portable_workflow.py#unit-01`,
  `skills/_shared/review_terminal.py#unit-01`; rulebook v4.
- Class: `library`. Disposition: open, affected units remain partial. This is
  not an approved contract change. No command is switched over.
- Oracle/source revision: `e897b10d58b27a46544969594eed08a5ed64c72c`;
  Python 3.14.3. Candidate/judge: the workflow crate and its source-oracle
  tests in PR #1307, pinned Rust 1.94.1. Config is fixture data, not a live
  installation. J1/J2/J3 cut-over coverage remains outstanding.
- Reproducer: Python `json.loads('{"unused": NaN}')` returns a dictionary;
  serde_json's current workspace features refuse that token. Python also
  accepts `"\ud800"` inside an otherwise unused context field or terminal
  path; Rust `String` cannot represent a lone surrogate. Non-finite numbers
  and lone surrogates remain parser integration gaps. Integer overflow is
  handled separately as improvement RP-1005.
- Implemented scope: identity validation, digesting, portable workflow effects
  on finite scalar JSON, and terminal decisions for representable input
  values. The temporary JSON object reader lives in the private `pending`
  module and is marked for `provenant-config`.
- Follow-up: the config/records owner must supply an accepted representation
  and parser API, or obtain explicit approval for a restricted boundary.
  Do not expand serde features or claim full port/cut-over parity locally.
  [Crate status](../../../tools/front-door/crates/workflow/PORT_STATUS.md)
  records other unported custody effects and judge gaps.

## RP-1005: Python integers outside i64/u64

- Units: `skills/_shared/portable_workflow.py#unit-01` and
  `skills/_shared/review_terminal.py#unit-01`; rulebook v4.
- Class: `improvement`. Python accepts arbitrary-size integers while the Rust
  JSON boundary intentionally supports only i64/u64.
- Compared revisions: Python source oracle
  `e897b10d58b27a46544969594eed08a5ed64c72c`; Rust workflow reader at this PR
  head.
- Reproducer: context JSON or review-dispatch JSON containing
  `18446744073709551616`; the checked JSON boundaries return typed
  `NumberOutOfRange` instead of rounding the integer through `f64`.
- Expected / actual: Python retains this as an arbitrary-size `int`; Rust
  refuses it with typed `NumberOutOfRange`, without returning a rounded value.
- Evidence: the portable workflow and review terminal regression tests
  exercise the exact JSON token and assert the typed error variants.
- Disposition: recorded rulebook v4 improvement divergence. Review terminal
  remains partial for the separate lone-surrogate parser gap in RP-1003.
- Follow-up: move the checked codec to `provenant-records` and preserve the
  typed refusal at its JSON boundary.

## RP-1004: workflow malformed container and Unicode parity repairs

- Units: review ladder, terminal review, run gates and portable identity unit
  01; rulebook v4 and oracle revision as in RP-1003.
- Class: `bug`. Disposition: fixed in this batch's review repair.
- Reproducer/evidence: crate differentials include a list/dict as a status,
  issue severity or read-only guarantee, control whitespace U+001C through
  U+001F, and non-ASCII Git object strings. The first Rust draft treated
  unhashable policy fields as ordinary false/failed values and used Rust's
  narrower whitespace and byte-width checks. Python raises a TypeError for
  unhashable set members, strips those controls and counts Unicode code
  points for object widths.
- Repair: typed failures retain the source's short-circuit precedence;
  whitespace and object widths follow Python. Only native exception prose
  is normalised to exception type plus container kind across CPython
  versions. Source policy messages, JSON artifact bytes and result fields
  are compared unchanged. No intentional behaviour changes are accepted.
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

## RP-1301 - Provider authentication and probe compatibility repair

- Units: `skills/orchestrate/scripts/adapters/claude.py#unit-01` and
  `skills/orchestrate/scripts/adapters/kiro.py#unit-01`, rulebook version 4
  (`origin/docs/porting-v4`, PR #1297).
- Class: `bug` (fixed) and `library` (fractional UTC-offset rounding, fixed).
- Compared revisions: unchanged Python oracle at
  `a7432845a039ad453a488191f6a1b7c46fd40b5f`; pre-repair Rust at
  `fc14cd5f75da11b22581fdaaf06c451e94d462f9`; repaired source and judges in
  PR #1298. The PR body pins the repair head and its checks.
- Reproducer: `PROVENANT_PORT_DIFF_REQUIRED=1 cargo test -p provenant-providers -j 2`
  in `tools/front-door`, macOS arm64, Python 3.14.8. Oracles clear the
  environment and retain only PATH plus an explicitly selected synthetic
  ANTHROPIC_API_KEY; no provider/config state or temporary paths are used.
- Expected / actual: Python's full `uses_api_key` predicate was represented
  only by a caller-supplied boolean; endpoint-only fixtures hid both OAuth
  and key-based authentication branches. Python accepts truthy JSON CLI
  versions with Python equality (including `2 == 2.0`), while Rust required
  strings. Python truncates timestamp fractions beyond six digits. Jiff also
  rounds `+00:00:00.500000` to a whole-second offset: for
  `2026-10-23T12:00:00+00:00:00.500000`, expected epoch microseconds are
  `1792756799500000`, but the original parse gives `1792756799000000`.
- Evidence: the added numeric-version / timestamp-boundary regression failed
  with `prompt_only` versus `enforced` before repair. The exact timestamp
  differential independently failed on the fractional offset above. Both
  pass after repair, along with 180 argv comparisons, 36 predicate
  comparisons, 2,093 probe comparisons and eleven exact timestamp cases.
  Basic ISO date/time forms are included. Integers stay i64/u64; comparisons
  never round integers through f64, and timestamps use integer microseconds.
- Disposition: fixed; no intentional contract change. Exact-head CI and
  independent J1/J2/J3 cut-over gates remain pending.
- Follow-up: move JSON truth/equality and ISO timestamp adaptation from the
  crate's `pending` module to `provenant-core` during integration. The CLI
  pending module has repr/strip/digit helpers, but no JSON truth/equality or
  ISO timestamp implementation to copy; these are extensions for core.

## RP-1302 - Kiro fromisoformat parser coverage

- Unit: `skills/orchestrate/scripts/adapters/kiro.py#unit-01`, rulebook version 4.
- Class: `library` (open; not an approved exception).
- Compared revisions: Python oracle at
  `a7432845a039ad453a488191f6a1b7c46fd40b5f`; Rust parser and differential
  fixture in the PR #1298 repair head pinned in its body.
- Reproducer: `cargo test -p provenant-providers -j 2 recorded_fromisoformat -- --nocapture`
  in `tools/front-door`, macOS arm64, Python 3.14.8. The fixture creates a
  fresh probe one hour before its explicit microsecond clock and replaces
  the timestamp's `T` with U+1F980 (crab).
- Expected / actual: Python accepts an arbitrary single-character date/time
  separator and returns `enforced`; Jiff rejects this spelling, so Rust
  returns `prompt_only`. Basic format, long fractions, comma fractions and
  fractional UTC offsets are adapted and compared separately in RP-1301.
- Evidence: the retained differential asserts the exact old/new discrepancy;
  it is mismatch evidence, not a passing parity claim. Other fromisoformat
  grammar differences have not been exhaustively enumerated.
- Disposition: open. Kiro is partial and excluded from the PR's `Units:` line.
  No runtime cut-over or exception approval is requested.
- Follow-up: adapt the remaining Python ISO grammar and replace the known
  mismatch assertion with a parity assertion before claiming this unit.

## RP-1303 - Unsigned provider plan fields narrow Python numeric inputs

- Units: `skills/orchestrate/scripts/adapters/agy.py#unit-01` and
  `skills/orchestrate/scripts/adapters/claude.py#unit-01`, rulebook version 4.
- Class: `bug` (open; not an approved improvement).
- Compared revisions: Python oracle at
  `a7432845a039ad453a488191f6a1b7c46fd40b5f`; unsigned `Plan` fields retained
  in the PR #1298 repair head pinned in its body.
- Reproducer: call the unchanged Python `agy.argv` with otherwise-valid plans
  and `timeout_seconds` set to `1.9`, `"30"`, and `-5`. The respective
  `--print-timeout` arguments are exactly `1s`, `30s`, and `1s` because Python
  applies `max(1, int(value))`. Rust's `u64` plan decoder refuses these values.
  With `context_ceiling=-1`, Python `claude.argv` emits
  `--autocompact -1`; Rust's `Option<u64>` cannot represent the input.
- Evidence: reproduced with Python 3.14.8 in an environment containing only
  PATH on macOS arm64. The supported unsigned-input cases remain covered
  by argv differentials; these decoder gaps are not claimed as parity.
- Disposition: open. Agy and Claude remain partial and are excluded from the
  PR's `Units:` line until decoding/conversion is ported. The unsigned API
  is retained for the inert crate only; it is not a new dynamic-plan contract.
- Follow-up: port the input conversions at the typed boundary, preserving
  Python ints as i64/u64. The new checked JSON boundary already refuses
  values outside that range with typed `NumberOutOfRange` (RP-1304); it does
  not yet implement the timeout/context coercions. The crate's `Plan` rustdoc records
  the flattened `applied.add_dirs` / `route.endpoint_base_url` mapping.

## RP-1304 - Provider JSON integer range refusal and malformed offsets

- Units: provider JSON input boundary and
  `skills/orchestrate/scripts/adapters/kiro.py#unit-01`, rulebook version 4.
- Class: `improvement` for the required out-of-range refusal (R4); `bug` for
  the fractional-offset overacceptance (fixed).
- Compared revisions: Python oracle at
  `a7432845a039ad453a488191f6a1b7c46fd40b5f`; Rust repair and native review
  follow-up in PR #1298, exact candidate pinned in its body.
- Reproducer: `cargo test -p provenant-providers -j 2 json_boundary` and
  `cargo test -p provenant-providers -j 2 iso_timestamp_conversion` in
  `tools/front-door`, macOS arm64, Python 3.14.8.
- Expected / actual: Python keeps `18446744073709551616` and
  `18446744073709551617` as distinct ints; serde_json's unchecked Value
  decoder rounds both to the same float. The new checked JSON boundary
  refuses these tokens with `DecodeError::NumberOutOfRange` before serde
  parses them, as R4 requires. In-range signed/unsigned endpoints stay exact;
  decimal/exponent JSON numbers remain floats. Pre-parsed Value APIs require
  exact values and cannot recover information already lost by another decoder.
  Python rejects offset `+01:02.500000`; the first fractional-offset repair
  accepted it. Fractions now require explicit offset seconds, matching Python.
- Evidence: native adversarial review found both issues; regression tests
  failed on the unchecked oversized integer and malformed fractional offset
  before the fixes. Tests cover the checked Kiro JSON entry point, both range
  endpoints, integers above 2**53, integer-looking strings, and invalid offset
  fractions after only hours or minutes. All pass after repair.
- Disposition: fixed; oversized integer refusal follows the user's explicit
  R4 instruction. No broader numeric input narrowing is approved (RP-1303).
- Follow-up: move the checked JSON token preflight from `pending` to
  `provenant-records` when its codec lands; use `decode_json` or
  `read_only_guarantee_json` at dynamic input boundaries.
## RP-2001 - Quarantine paths lost Unix filename bytes

- Unit: `skills/orchestrate/scripts/instruction_quarantine.py#interface`,
  rulebook version 3.
- Class: `bug` (fixed; no intended contract change).
- Reproducer: `cargo test -j 2 -p provenant-confinement non_utf8_instruction_names_survive_restore_and_index_framing`
  from `tools/front-door`, macOS arm64, Python oracle selected by `python3`.
  Fixture paths contain literal bytes `ff` and `fe` below `.agents/`.
- Compared revisions: Python oracle
  `9b2da713b4cc3b188b1c8697eb7c6755ec0d8b0d`; faulty Rust interface
  `d91b9dfbc1e744f7ee70d36f0fdfafe893050726`; repair is the commit adding this entry.
- Expected / actual: Python's surrogateescape round trip retains these bytes
  in restore-view keys and NUL-framed index input. Rust `String` keys could
  not represent them and `.as_bytes()` could not reproduce their framing.
- Evidence: native adversarial review identified the restriction. The added
  regression exercises integration, restored views, quarantinability and
  byte-for-byte framing against Python `_index_info` on the same fixture.
- Disposition: fixed using `OsString` keys and raw Unix bytes. Snapshot values
  are in-memory interface values; they emit no JSON here. The record owner
  must supply the Python-compatible filename wire encoding before persistence
  is integrated. No replacement JSON encoding is introduced.
- Follow-up: retain the differential regression. Independent J2 schema and
  mutant evidence remains a cut-over gate; this lane does not claim it.

### RP-2001 follow-up - Preserve index caller order

- Repair review found a second framing mismatch: `BTreeSet<OsString>` sorted
  raw bytes, while the Python runtime sorts surrogateescaped strings. Valid
  U+0080 sorts before surrogateescaped byte `80` in Python; raw byte order
  reverses them. Both produce the same Git index, but different input bytes.
- Class: `bug` (fixed). Faulty repair: `c8eaafd0`; oracle unchanged from RP-2001.
- Disposition: `index_info` now takes an ordered slice, preserving its caller's
  order exactly as Python `_index_info` does. The later quarantine runtime
  supplies Python-sorted changes; the interface does not silently sort them.
- Evidence: `index_framing_preserves_python_caller_order_for_mixed_names`
  compares the mixed-name framing with Python and checks caller order is
  retained. This supplements the original invalid-byte regression.
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
### RP-RUN-1 — lease compatibility limits (open)

- Unit: `skills/orchestrate/scripts/lease.py#unit-01`; rulebook v2 with
  the v3/PR #1294 inert-crate gate override from the lane brief.
- Class: `library`. Oracle: `e897b10d58b27a46544969594eed08a5ed64c72c`.
- Reproducer: supply a generation outside signed 64-bit range, a non-string
  historical holder field, or a Python-only `datetime.fromisoformat` spelling.
  Python's dynamic values and unbounded integers exceed this first typed API;
  Rust refuses unsupported fields/timestamps/generations explicitly.
- The pending reader also bounds lease and handoff inputs at 1 MiB; the
  Python lease reader has no bound. Oversized lease input returns an I/O
  error; oversized handoff input cannot authorise takeover.
- Evidence: crate policy tests and a fixed-clock Python differential cover
  acquisition, contention, renewal refusal, transfer, release, record bytes,
  Unicode escaping and private publication permissions. They do not cover
  the compatibility limits above.
- Disposition: open; partial unit, inert only. These limits are not approved
  boundary changes and must be reconciled before cut-over. CLI argparse and
  exact parser/I/O diagnostics remain with the future CLI owner/J1.

### RP-RUN-2 — detached-worker integration and reader limits (open)

- Unit: `skills/orchestrate/scripts/run_worker_detached.py#unit-01`;
  rulebook v2 with the v3/PR #1294 inert-crate gate override.
- Class: `library`. Oracle: `e897b10d58b27a46544969594eed08a5ed64c72c`.
- Reproducer: feed non-ASCII digit PID fields, invalid UTF-8, or files over
  1 MiB to the evidence reader. The first Rust API accepts ASCII digits,
  treats non-UTF-8/oversized evidence as unavailable and bounds reads;
  Python uses `str.isdigit`, locale UTF-8 decoding and unbounded `read_text`.
- Evidence: fixed-clock Python validation matrix, real child wait/reap,
  merged descriptors, null stdin, signal exits, concurrent claim, exec
  failure fallback, permissions and failed publication fixtures in the crate.
- Disposition: open; partial unit, inert only. Safe fresh-session spawning,
  process-existence probes, access checks and exact OS error text are
  `pending` supervisor contracts. Fixture child processes verify lifecycle
  and descriptor policy, not session isolation or wrapper-death survival.
  The production supervisor adapter and surviving OS conformance remain
  before cut-over. This does not approve a boundary change.

### RP-RUN-3 — native review corrections (fixed)

- Units: `skills/orchestrate/scripts/run_lineage.py#interface`,
  `skills/orchestrate/scripts/run_worker_detached.py#unit-01`; rulebook v2
  with the v3/PR #1294 inert-crate gate override.
- Class: `bug`. Oracle: `e897b10d58b27a46544969594eed08a5ed64c72c`.
- Reproducer: a failed child's `error: false` incorrectly printed `False`
  instead of falling back to `failed`; the first ownership interface lost
  Python's standalone batch-child result; the test worker cleanup sent
  SIGKILL although Python requests SIGTERM after a PID publication failure.
- Expected/actual: preserve JSON truthiness for the child error fallback,
  retain standalone/shared execution-custody decisions, and send SIGTERM
  only to the owned worker before reaping it.
- Evidence: native source review; explicit falsey error and custody-mode
  regression fixtures; a FIFO-synchronised SIGTERM trap fixture. Source
  `tests/test_lease.py` and `tests/test_worker_completion_guidance.py`: 14 pass.
- Disposition: fixed in `ba8fdbea` and `5cddf0af`; independent repair review
  found no remaining high-confidence issue. Pending production OS behaviour
  and the RP-RUN-1/RP-RUN-2 compatibility limits remain open.
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
- Disposition: accepted as a correction (chair, 2026-10-23, user informed).
  Callers are `clean.py` run and attempt cleanup and attempt `tmp/` pruning,
  whose planned paths never contain `..`; worktree pruning goes through
  `scripts/worktree remove`, not `remove_tree`, so automatic post-merge pruning
  is unaffected. The refusal only removes the ability to delete outside the
  anchor, which the containment promise already forbids.
- Follow-up: integrate supervisor's safe OS binding and run descriptor-based
  adversarial conformance on both platforms.

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
## RP-config-4 — config command edge rendering gaps

- Units: `scripts/configure-codex-skill-overrides.py#unit-01`,
  `scripts/install-workflows#unit-01`; rulebook v4.
- Class: `bug`. Disposition: open; affected units remain partial. These
  mismatches are not approved contract changes.
- Compared revisions: Python source revision
  `7475ff01e437ec51c760b852516c21a5281cf9c6`; Rust command boundaries are
  commits `8638ee4ab59b8581b7a7ca80d06ed1c26cefb435` and
  `4dc3cc45ea474754deb828f4d76a559ada9addc3`. Python 3.14.3. Fixture roots
  are isolated temporary directories; no live config is read or written.
- Reproducers and expected/actual:
  - Codex config with `enabled = []`: Python exits 1 with an uncaught
    `TypeError` traceback; the Rust command result exits 1 with a short
    `TypeError` line.
  - On Unix, a `--config` or workflow `--target` path containing a non-UTF-8
    byte is preserved in Python's printed bytes; current Rust `Path::display`
    replaces it with U+FFFD, and command result strings cannot preserve that
    byte sequence.
- Evidence: reviewer source comparison against
  `scripts/configure-codex-skill-overrides.py:44,132-134`, and
  `scripts/install-workflows:290-292`; Rust implementations are in `codex.rs`
  and `workflows.rs`. `codex::tests::unhashable_enabled_values_keep_python_type_error_and_exit_status`
  compares the source exception and verifies status, while
  `workflows::tests::conflicting_preflight_command_matches_python` compares
  command streams and status. The traceback bytes and non-UTF-8 path output
  remain uncovered.
- Follow-up: add exact command edge rendering and OS-path compatibility before
  claiming these units complete. No command cut-over is included in this lane.

## RP-config-5 — workflow JSON parse diagnostic wording

- Unit: `scripts/install-workflows#unit-01`; rulebook v4.
- Class: `library`. Disposition: open; the unit remains partial. This is not
  an approved contract change.
- Compared revisions: Python source revision
  `7475ff01e437ec51c760b852516c21a5281cf9c6`; Rust workflow reader commit
  `4dc3cc45ea474754deb828f4d76a559ada9addc3`. Python 3.14.3. Fixture roots are
  isolated temporary directories; no live receipt is read or written.
- Reproducer: create an empty `.agent-harness-workflows-installation.json`
  beside the target and load it through `_load_manifest` and `workflows::load`.
- Expected / actual: both reject the receipt. Python reports
  `Expecting value: line 1 column 1 (char 0)`; serde_json reports its EOF
  parse diagnostic and byte offset. The command status remains 3.
- Evidence: source handling is at `scripts/install-workflows:67-70`; Rust
  parsing is at `workflows.rs:78` through `pending::read_json`. The malformed
  receipt differential remains to be added; only successful receipt and
  command cases plus conflict refusal are covered in this batch.
- Follow-up: reconcile Python JSON diagnostic rendering at the command edge
  and add malformed receipt failure differentials before claiming the unit
  complete.

## RP-config-6 — instance interface input ordering and home expansion

- Unit: `scripts/instance_installation.py#interface`; rulebook v4.
- Class: `bug`. Disposition: open; the interface remains partial. These are
  not approved contract changes.
- Compared revisions: Python source revision
  `7475ff01e437ec51c760b852516c21a5281cf9c6`; Rust interface commit
  `742ce1d92ac8632e3a673e63cdc2999594d5bdec`. Python
  3.14.3. The path traversal case uses an in-memory fixture; the `~username`
  case depends on the host account database and does not touch instance files.
- Reproducers and expected/actual:
  - Validate the JSON text
    `{"schema_version":1,"contract":"installation-desired-state","product":{"version":"${ROOT}","name":"~/product"},"mode":"split"}`.
    Python retains product insertion order and reports `/product/version`
    first; conversion into serde_json's default sorted object reports
    `/product/name` first.
  - Set `AGENT_FABRIC_INSTANCE_ROOT=~mikey/instance` on a host where `mikey`
    exists. Python `Path.expanduser()` returns that account's absolute home
    path; Rust expands only `~` and `~/`, then refuses the relative value.
- Evidence: source review at `scripts/instance_installation.py:191-203,737-743`
  and candidate implementation at `instance.rs:57-67,134,188-194`. Existing
  desired-state differential cases cover representable values but not these
  two cases; a retained exact differential remains to be added.
- Follow-up: expose an ordered JSON input representation and platform home
  directory lookup before claiming this interface complete. No caller
  cut-over is included in this lane.

## RP-FR2-001: Fabric policy ordered mappings and Python repr (open, library)

- Unit: `skills/orchestrate/scripts/fabric_policy.py#unit-01` (partial),
  rulebook v4.
- Compared source/oracle revision: `600dec09bcda3b6a86272e391420e89d38777953`.
- Reproducer: configure `tools` with keys in the order `z`, then `a`, or use
  an invalid `secret_scan_exclude` entry containing the float `1e-7` or a
  nested object with reordered keys. CPython preserves dict insertion order
  and renders `1e-7` as `1e-07`; this crate's `serde_json::Map` sorts keys and
  its current Python repr helper does not preserve source object order or
  CPython float spelling.
- A second open edge is a non-finite JSON number in an unknown policy field.
  Python's JSON decoder accepts `NaN` and infinities. The pending checked JSON
  adapter maps them to `null` so the source-compatible typed `default_branch`
  warning is preserved, but that changes unknown-field values and is not a
  general Python JSON number representation.
- Non-finite values in `secret_scan_exclude` also remain different: for
  `secret_scan_exclude: [NaN]`, Python warns with `nan`, while the Rust warning
  currently renders `None` after the adapter maps the value to null.
- Exact `repr()` parity also remains open for quote selection and control
  escapes. For example, Python uses double quotes for a value containing an
  apostrophe when that avoids escaping, and renders U+0001 as `\x01`; the
  pending helper always uses single quotes and does not encode all control
  characters this way.
- Evidence: policy differential tests cover defaults, invalid scalar entries,
  containment, warning text and `NaN` on the typed `default_branch` field, but
  do not exercise the above ordered/repr cases, non-finite exclusion entries
  or non-finite unknown values.
  The difference is visible in returned tool settings and warnings.
- Disposition: open; no behaviour change is accepted. Keep the unit partial
  until the `provenant-records` ordered Python-value codec and
  `provenant-core` repr helper are available to this crate.


## RP-core-2-01 — shared Python compatibility ownership and ISO repairs

- Rulebook v4; class `bug` (fixed, no intentional source behavior change).
- Source/oracle: Python sources at `7475ff01`; copied helpers from quota's
  verified pending module at that revision and providers PR #1298 revision
  `4155f202e01c9e275e24a9d188f9be5839c04008`. Local CPython 3.14.8,
  Unicode 16.0.0; Rust oracle runner enforces Python >=3.12.
- Reproducer: `py::tests::iso_grammar_and_offsets_match_python`, including
  `2026-W43-5112`, Unicode separators, ISO week dates, fractional UTC offsets,
  and `9999-12-31T12Z`. The copied Jiff adaptation did not cover this grammar
  and Jiff's UTC timestamp range excludes Python's final twelve hours.
- Repair: shared `py` parses Python civil datetimes and retains exact i64 UTC
  microseconds in `PythonTimestamp`; `to_jiff` is explicitly checked. Week-date
  separator disambiguation follows CPython's local `_pydatetime` implementation.
  Float-second conversion retains quota's modf/half-even implementation.
- Evidence: exact differential grammar matrix, 1,033 fromtimestamp fixtures,
  numeric equality across bool/int/float, 512 deterministic float bit patterns,
  and all Unicode scalar printable classifications. No epsilon, timestamp
  normalisation, or integer-to-float comparison is used.
- Disposition: source compatibility gathered into `provenant_core::py`;
  other crates' copies remain for their owners to migrate. Checked JSON stays
  in records. JSONC/host/pointer representation gaps remain separately open.

### RP-1001 core-2 follow-up — byte encodings and slug inputs

- Unit: `scripts/host_identity.py#unit-01`; rulebook v4; class `bug` (partial
  repair, no intentional behavior change). Source/oracle `7475ff01` unchanged
  at main `cc02dde4`; route `codex/gpt-6.1-sol@high`.
- `configured` and lenient `peer_ids` now use Python's BOM and null-byte sniffing
  for UTF-8/16/32, including BOM-less UTF-16/32. Decode errors preserve typed
  causes and the source's stable `InvalidHost` message.
- `slug_value` applies the shared Python truth/str helpers for JSON-shaped
  inputs; the hostname observation API continues taking a string value.
- Evidence: exact Python differential over eight valid byte encodings and
  eight malformed/shape/duplicate cases, plus thirteen permissive slug inputs.
- Remaining: nonfinite/overflow values and lone surrogate JSON strings (and
  raw surrogate-pass byte decoding). The existing failing parity reproducer
  remains ignored; this unit is still partial. No codec was moved from records.

### RP-1002 core-2 follow-up — comment diagnostics and shared whitespace

- Unit: `scripts/lib/jsonc.py#unit-01`; rulebook v4; class `bug` (partial
  repair). Source/oracle `7475ff01`; route `codex/gpt-6.1-sol@high`.
- Unterminated-comment error display now includes Python's exact line, column
  and character offset. Unicode and CRLF differential fixtures compare all
  four observable diagnostic values, including errors after earlier comments.
- Trailing-comma lookahead uses the shared `py::python_whitespace`; control
  whitespace is still rejected by the JSON grammar just as Python rejects it.
- Remaining: extended JSON/surrogate/insertion-order representation, exact
  integer ingress and generic JSONDecodeError diagnostics. Core cannot depend
  on records under R2; no checked codec is moved or copied by this repair.
  The unit remains partial and is excluded from `Units:`.

### RP-1003 core-2 follow-up — native pointer writer bytes

- Unit: `scripts/lib/product_root_resolver.py#unit-01`; rulebook v4; class `bug`
  (partial repair). Source/oracle `7475ff01`; route `codex/gpt-6.1-sol@high`.
- Pointer writing now emits Python's ASCII JSON spelling for native paths,
  including U+007F and Unix surrogateescape for invalid UTF-8 bytes. It never
  converts a path through replacement characters. Publication policy is unchanged.
- Evidence: exact path-wire differential over 260 native byte fixtures, plus
  pointer-writer differential for a real non-UTF-8 directory when supported.
  The pure byte fixtures always run even if the filesystem refuses that name.
- Pending: the small private `pending::path_json` and its error type belong in
  records, whose current API cannot represent surrogateescaped JSON paths.
  No checked integer codec is moved or copied; R2 prohibits a core->records
  dependency. `pending::atomic_write` continues awaiting supervisor publication.
- Remaining: pointer loading for surrogateescaped paths, extended JSON in
  ignored fields, and exact integer ingress. The retained ignored differential
  still documents these reader gaps; the unit remains partial.

## RP-core-2-02 — scripts root facade completion

- Unit: `scripts/lib/roots.py#unit-01`; rulebook v4; class `bug` (fixed).
  Source/oracle `7475ff01`, unchanged at `cc02dde4`; route
  `codex/gpt-6.1-sol@high`.
- The Rust `roots` exports are the statically linked equivalent of the Python
  facade's re-exported constant and two functions. Shared-root policy was already
  present; direct facade differentials now cover unset/empty overrides, tilde,
  lexical path spelling, unknown-user errors and native environment path bytes.
- A right-reason failing differential exposed a missing final period in
  `PathError::Home`; its wording now matches Python's expanduser RuntimeError.
- All runtime policy is ported. Python importlib loading/defensive missing-module
  branches are interpreter plumbing, replaced by Rust's compile-time module
  linkage; no Python loader seam is copied. No command is switched.
- Evidence: five roots tests pass, including the two direct-facade
  differentials; default install paths use canonical paths on macOS.

### RP-core-2-01 oracle patch-version clarification

- CI uses CPython 3.14.4; local default is 3.14.8. Both accept midnight and
  require explicit seconds before fractions, but disagree on `-00:00:00.5`:
  3.14.4 discards the fractional offset, while 3.14.8 retains it. Requiring
  >=3.14.8 initially caused the required CI differential to refuse its oracle.
- Preserve both observed behaviors through `IsoformatSemantics` and
  `python_fromisoformat_with`; the default matches pinned CI Python 3.14.4.
  The exact differential chooses the source interpreter's explicit semantics
  from an explicit datetime feature probe, never normalising timestamp results. Both 3.14.4 and
  3.14.8 are installed/run locally. No Python bug is silently fixed.
- The grammar fixture requires >=3.14; Python 3.12/3.13 remain valid for the
  other oracle fixtures. A missing/incompatible required oracle still fails.

### RP-1001 review correction — preserve slug input order

The core-2 review found that the newly proposed `slug_value(&Value)` boundary
would render sorted serde mappings rather than Python's insertion order
(`{'z': 0, 'a': 0}` must slug to `z-0-a-0`). That generic API and its scalar-only
fixture were removed before landing. The string/hostname slug API remains;
generic Python input slugging awaits an ordered value representation and is
explicitly part of this unit's remaining partial scope. The byte-encoding
repair above is retained and independently covered.

### RP-1003 review correction — unknown home errors

A direct source differential exposed an ordinary pointer-loading error path:
Python's `Path.expanduser` RuntimeError for an unknown user is not caught by the
source reader. The old Rust reader discarded it as pointer absence. Both Rust
load APIs now return `Result<Option<PathBuf>, PathError>` and preserve that error;
malformed/missing pointers still return `Ok(None)`. No callers outside core use
these inert APIs. The regression failed with `null` versus the source error,
then passed after repair. Extended JSON/native-path reader gaps remain partial.

## RP-FS2-001: diagnostic count-key order (bug, fixed)

- Units: `runtime/fabric/src/store.ts#unit-04` (partial), rulebook v4.
- Source/oracle: `600dec09bcda3b6a86272e391420e89d38777953`, Node v24.15.0,
  better-sqlite3 13.0.3. The source inserts `agents`, `messages`,
  `pendingDeliveries`, `tasks`, `activity`; CLI JSON preserves that order.
- Native source review caught Rust's initial sorted map changing these bytes.
  `DiagnosticCounts` now serialises fields in source order. Every differential
  result also carries the original record's serialised JSON for byte comparison,
  so parsing the surrounding fixture JSON cannot hide record-key reordering.
- Evidence: `fabric-store` diagnostic policy differential, including namespace
  collision failure. Fixed; no intentional divergence or cut-over accepted.

## RP-FS2-002: JavaScript Date ISO range (library, fixed)

- Units: `runtime/fabric/src/store.ts#unit-02` and `#unit-03` (partial), v4.
- Same source/oracle as RP-FS2-001. `Date.toISOString` accepts TimeClip's
  inclusive +/-8640000000000000 ms limits and signed six-digit years outside
  0000 through 9999. Jiff's timestamp range is narrower.
- `pending::js_iso`, marked for `provenant-core`, formats integer Gregorian
  dates across the source range and retains `Invalid time value` outside it.
  The seven-scenario differential suite includes exact ISO output/errors at
  both bounds, year 0, year 10000, negative milliseconds and a leap date.
- Fixed by adapting the formatter; no widened dependency or behaviour change.

### RP-1302 follow-up: Linux census iteration errors (fixed Rust bug)

- Class: `bug`. Source/oracle: `7475ff01e437ec51c760b852516c21a5281cf9c6`;
  rulebook v4. `process_info.processes()` consumes the `/proc` iterator outside
  its open-error fallback. A later iteration OSError propagates to callers.
- Rust used `ReadDir::flatten()`, silently converting an incomplete census to
  success. It now preserves the underlying I/O error through the observation
  decoder. A fixture with one readable PID followed by EACCES failed before
  repair and passes afterwards; it runs on macOS as well as Linux.
- No intentional source behavior change. The wider RP-1302 fallback error and
  size-bound differences remain open; this does not complete the unit.

### RP-SUP-PS-BUFFER: Fabric ps maxBuffer capture (open partial scope)

- Class: `library`; not accepted. Source/oracle baseline:
  `7475ff01e437ec51c760b852516c21a5281cf9c6`, Node 24.21.0, rulebook v4.
- Node `execFileSync` uses a pipe and exposes overflow-buffer bytes plus
  `code`, `status`, `signal` and `stdout` on ENOBUFS. A 1,048,577-byte fixture
  exposed status 0 and 1,048,577 captured bytes. The Rust utility runner uses
  a file-backed spool, polls its size and retains a capped head/tail view.
  It can overshoot the threshold on disk between polls and does not reproduce
  Node's exact overflow stdout or associated termination observation.
- Native review reproduced the mismatch and blocked a full claim for
  `runtime/fabric/src/ps.mjs#unit-01`. Typed child errors now retain code,
  exit status separately from signal, stdout size and truncation evidence;
  signal/timeout fixtures compare directly against Node. This does not fix
  the remaining maxBuffer mismatch. Signal-name mapping currently covers
  common POSIX signals; platform-specific extensions also remain incomplete.
- Disposition: keep the unit partial and absent from `ported.txt`/PR `Units:`.
  No command uses the crate. Completing the unit requires a bounded pipe
  adapter and exact overflow/failure differentials before cut-over; no
  behavior change is accepted here.

### RP-SUP-FS-ERRNO: portable filesystem error contract (fixed)

- Class: `bug`; ungraphed supervisor filesystem foundation, PR #1328 repair.
  Author route: `codex/gpt-6.1-sol@high`. Main baseline:
  `d8be7244ada4b5e7a9dadc565a7cab292e54890f`.
- Reproducer: no-follow directory open on a final symlink reports ENOTDIR on
  Linux and ELOOP on macOS. The previous tests asserted only failure, leaving
  consumers unable to distinguish links from ordinary non-directories.
- Fixed contract: classify ENOTDIR/ELOOP with no-follow fstatat in the syscall
  layer; a positively observed final symlink returns typed `FsError::Symlink`
  and canonical ELOOP. Ordinary non-directories retain ENOTDIR. `io::Error`
  conversion preserves the resulting errno/kind; typed errors retain complete
  anchor-relative diagnostics. Observed anchor-device crossings report EXDEV.
- Evidence: `directory_symlinks_have_a_portable_typed_errno_and_nested_context`,
  `typed_error_predicates_and_io_conversion_preserve_errno_and_kind`, and
  tightened final-link/anchor fixtures assert variants and errno. Publication
  and regular-file flag regressions cover the same repair. Required local
  supervisor tests pass on macOS; Linux execution remains a CI gate because
  the cross-compilation target is not installed.
- Scope: public filesystem traits and durable publication helpers now support
  adapters for the pending confinement/records/config/core contracts. Consumers
  remain unchanged; no graph unit or full conformance claim is added. README
  maps the remaining pending run and UUID contracts explicitly.

### RP-PROVIDERS-3-OPENCODE: #1322 review repairs (fixed Rust bugs)

- Class: `bug`. Rulebook v4; source/oracle baseline
  `b527d675d15c50ee51701bb15db6439c95277918`;
  author route `codex/gpt-6.1-sol@high`.
- The first lookaround-valid version decides, even when >=2 or larger than
  u64; Unicode decimal digits follow Python semantics. `opencode 2.0.0
  (bun 1.1.3)` now returns None. Lowercase diagnostics compare levels after
  uppercasing; empty models omit --model; EXIT_AFTER_OUTPUT is in the profile.
- Registry rows now own metadata, constructor and optional output hooks once,
  in source NAMES order. Differential fixtures pin each reported edge. No
  intended Python behavior change.

### RP-1302 / RP-1303 providers follow-up (fixed Rust bugs)

- Class: `bug`. Same baseline/rulebook/route as above. Adopting core's Python
  ISO parser closes Kiro's arbitrary Unicode separator gap. Timeout/context
  inputs retain Python coercion and truthiness, including signed values,
  bools, fractional numbers, Unicode digits, repr and nested dictionary order.
- Python int(string) does not strip U+001C..U+001F even though str.strip does;
  this is covered separately from ordinary whitespace. Integer overflows are
  typed NumberOutOfRange, the already approved R4 `improvement`; no new
  numeric narrowing is accepted.
- Native review caught a truthy non-string event type triggering the `event`
  fallback, reset-time object traversal losing insertion order, an invented
  list for shared_write_paths, and receipt field order drift. All are fixed;
  source oracles cover the repaired cases, including exact dispatch bytes.

### RP-PROVIDERS-3-PARSER: remaining output parser coverage (open partial)

- Class: `library`; unaccepted. Same source baseline and rulebook v4.
- Python json.loads/json.dumps accept NaN/Infinity and deeper structures than
  the workspace's strict serde/checked_json boundary. Example: a result event
  containing NaN in an unrelated field is valid to the Python parser but
  rejected as structured JSON by the current Rust codec. No silent parity
  claim: both output port units remain partial and absent from ported.txt.
- Jiff's UTC Timestamp cannot cover Python's entire final day of year 9999;
  core's exact PythonTimestamp exists, but a full-range ISO renderer is not
  public. Reset-time endpoint and ambiguous/DST-local edge coverage remains
  incomplete. No cut-over or behavioral exception is approved.
- Remaining effect work: Codex discovery/state-path integration and model
  rollout scanning/OpenCode export need safe host APIs reachable through the
  allowed R2 dependencies. Supervisor owns safe filesystem/bounded execution;
  providers has no permitted direct edge or confinement bridge today. Keep
  these source functions unclaimed rather than copying the OS owner.
