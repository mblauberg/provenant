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
