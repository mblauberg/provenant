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
