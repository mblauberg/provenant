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

## RP-records-001 — Python adapter JSON numeric and whitespace semantics

- Unit: `skills/orchestrate/scripts/attempt_evidence.py#unit-01`; rulebook v3.
- Class: `library` (numeric parsing) and `bug` (blank-line classification).
- Reproducer: crate test `differential_python_success_matrix`, cases
  `nonfinite_unknown`, `overflow_unknown`, `negative_zero_adapter_exit`,
  `python_blank_tail`; macOS arm64,
  isolated temporary run directory, no environment/config normalisation.
- Compared revisions: Python oracle `9797088841644372af432f64b6dd3918bbdd5c19`;
  Rust before repair `3f471de7`; J2 baseline `d9fcebd4`. No independent mutant
  proof is claimed for these added differential cases.
- Expected / actual: Python accepts ignored `NaN`, infinities and `1e400`
  receipt fields; strict serde parsing refused them as invalid JSON. Python
  parses JSON `-0` as integer zero while serde retains floating negative zero. Python
  ignores a trailing U+001F-only line; Rust initially treated it as JSON.
- Disposition: fixed in this records batch. The validation projection maps
  nonfinite/beyond-range numbers to null (none can satisfy a numeric success
  field), without changing retained bytes, preserves integer `-0`, and uses Python
  blank-line rules.
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
