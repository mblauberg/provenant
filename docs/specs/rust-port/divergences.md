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
