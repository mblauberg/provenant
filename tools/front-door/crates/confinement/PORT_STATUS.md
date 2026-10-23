# Confinement port status

Rulebook: PORTING.md version 4 (#1297). Inert library only; no command owner
changes. Oracle sources at `9b2da713b4cc3b188b1c8697eb7c6755ec0d8b0d`
are unchanged from `e897b10d58b27a46544969594eed08a5ed64c72c`.

## Units

| Unit | Status | Boundary |
| --- | --- | --- |
| `skills/orchestrate/scripts/agy_home.py#interface` | Complete | Borrowed plan projection, staged-home identity, state names, overlay naming and modes, link exclusions and shim policy |
| `skills/orchestrate/scripts/instruction_quarantine.py#interface` | Complete | Snapshot entries, integration merge and HEAD/index/disk restore views, patch names and NUL index framing |
| `skills/orchestrate/scripts/lane_protection.py#interface` | Complete | Gate request, typed errors, release record, release path and naming, default base, and exact reviewed-tip preconditions |
| `skills/orchestrate/scripts/output_custody.py#unit-01` | Partial | Publication and verification algorithms implemented against pending safe filesystem API; no production backend or command parser |
| `skills/orchestrate/scripts/trusted_git.py#unit-01` | Complete | Owner registration, metadata validation, writer ref discovery and narrow grants |

The graph's separate `#unit-01` tasks for agy_home, instruction_quarantine and
lane_protection still own staging/copy-back effects, archive/restore effects
and history walking/release publication respectively. This batch does not
implement those runtime tasks.

## Pending integration

`pending::{CustodyFs, FileKind, Metadata}` belongs in `provenant-supervisor`.
Each item is marked in source. The pending module is private; the custody
module exposes its contract so the supervisor can supply a backend later.
The pending Python repr and Unicode printability table belong in
`provenant-core`; they are copied from the verified CLI helper until that
owner integrates them. `StateIdentity` uses the platform `st_dev` range (`u64`)
and nanosecond mtime (`i64`).
No unsafe or first-party libc calls exist here. The backend must provide
owned, descriptor-relative handles, no-follow opens, entropy, fsync, replacement
and cleanup; no path-based emulation is shipped as a runtime fallback.

The custody in-memory OS model tests policy, not the safety of any syscall
implementation. Real parent substitution during hashing, concurrent mutation,
sudden owner death and Linux/macOS filesystem conformance remain integration
gates. Custody CLI argument/help, exact I/O diagnostic rendering and exit
parity remain at the CLI cut-over boundary.

## Evidence and judge gaps

Crate tests translate policy assertions from `tests/test_provider_exec.py`
(agy and protected instructions), `tests/test_cf_dispatch.py` (custody), and
the trusted_git policy exercised by provider confinement. Tests use explicit
identities, fixture clocks and bytes. No assertion sleeps or patched globals.

Optional Python differential tests compare interface decisions, trusted Git
records/grants and error paths, lane revision diagnostics, non-UTF-8 quarantine
index framing and caller order, and custody publication bytes/digest/tamper
refusal. They use `PROVENANT_PORT_PYTHON` (default `python3`, Python 3.12+)
and skip with `skip:` when unavailable unless `PROVENANT_PORT_DIFF_REQUIRED=1`.
The custody differential deliberately compares
only digest, bytes, retained symlink target bytes and refusal; virtual and real
filesystem device/inode numbers are not compared. Source handles and OS
identity policy have separate model tests.

J1 entry-point/error/exit coverage, J2 release/snapshot schema and mutant
coverage, J3 caller transcripts, and real OS lane conformance have not run.
Per rulebook v4 these are cut-over gates, not evidence claimed by this inert
port. Cross-family review is chair-owned. The fixed filename-byte defect is recorded as RP-2001 in divergences.md.
Snapshot views remain in-memory; their persisted wire encoding belongs to the
later record integration. No intended behaviour divergences are accepted; no Python/TypeScript source is deleted.
