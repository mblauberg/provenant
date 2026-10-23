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

## RP-1001: workflow Python JSON representation gaps

- Units: `skills/_shared/portable_workflow.py#unit-01`,
  `skills/_shared/review_terminal.py#unit-01`; rulebook v2 with the
  user-directed v3 / PR #1294 inert-crate override.
- Class: `library`. Disposition: open, affected units remain partial. This is
  not an approved contract change. No command is switched over.
- Oracle/source revision: `e897b10d58b27a46544969594eed08a5ed64c72c`;
  Python 3.14.3. Candidate/judge: the workflow crate and its source-oracle
  tests at the PR head, pinned Rust 1.94.1. Config is fixture data, not a live
  installation. J1/J2/J3 cut-over coverage remains outstanding.
- Reproducer: Python `json.loads('{"unused": NaN}')` returns a dictionary;
  serde_json's current workspace features refuse that token. Python also
  accepts `"\ud800"` inside an otherwise unused context field or terminal
  path; Rust `String` cannot represent a lone surrogate. A dispatcher exit of
  `18446744073709551616` is an integer in Python and yields a terminal-available
  `nonzero-exit` leg; the current serde_json value boundary cannot retain that
  integer classification. `u64::MAX` is covered by the passing differential,
  but does not close the unbounded-integer gap.
- Implemented scope: identity validation, digesting, portable workflow effects
  on finite scalar JSON, and terminal decisions for representable input
  values. The temporary JSON object reader lives in the private `pending`
  module and is marked for `provenant-config`.
- Follow-up: the config/records owner must supply an accepted representation
  and parser API, or obtain explicit approval for a restricted boundary.
  Do not expand serde features or claim full port/cut-over parity locally.
  [Crate status](../../../tools/front-door/crates/workflow/PORT_STATUS.md)
  records other unported custody effects and judge gaps.

## RP-1002: workflow malformed container and Unicode parity repairs

- Units: review ladder, terminal review, run gates and portable identity unit
  01; rulebook and oracle revision as in RP-1001.
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
