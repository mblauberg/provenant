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
