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

## RP-1001 — supervisor partial numeric decoders

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
