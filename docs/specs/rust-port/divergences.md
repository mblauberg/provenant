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
