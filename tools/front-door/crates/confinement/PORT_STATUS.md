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
| `skills/orchestrate/scripts/output_custody.py#unit-01` | Partial | Identity-bound install/verify, supervisor safe filesystem backend, typed CLI request and renderer; FIFO source-open timing differs from Python pending a source-compatible owner API |
| `skills/orchestrate/scripts/trusted_git.py#unit-01` | Complete | Owner registration, metadata validation, writer ref discovery and narrow grants |

The graph's separate `#unit-01` tasks for agy_home, instruction_quarantine and
lane_protection still own staging/copy-back effects, archive/restore effects
and history walking/release publication respectively. This batch does not
implement those runtime tasks.

## Shared owners

`output_custody` now uses `provenant_supervisor::fs::{SafeFs, SafeDir, SafeFile,
Metadata, FileKind, LocalFs}`, `provenant_core::py::python_repr`, and
`provenant_records::checked_json`. The old pending filesystem and repr copies
are removed. A private OS-byte repr adapter remains in `pending.rs`, marked
for `provenant-core` because its Python layer does not expose surrogateescape
filesystem-name rendering.
Filesystem metadata uses the platform `st_dev` range (`u64`) and nanosecond mtime
(`i64`). No unsafe or first-party libc calls exist here. The supervisor backend
provides owned, descriptor-relative handles, no-follow opens, entropy, fsync,
replacement and cleanup; no path-based emulation is shipped as a runtime fallback.

The custody tests exercise real Linux/macOS filesystem operations through
`LocalFs`, including symlink replacement, identity and link-count checks,
mutation refusal, canonicalized temporary paths, CLI rendering, and Python
differential output and failure cases. FIFO source opening remains a known
partial boundary: Python's `os.open(source, O_RDONLY)` waits for a FIFO peer
before `fstat`, while the supervisor's `SafeFs::open_source` uses `O_NONBLOCK`
and refuses a FIFO immediately. A differential fixture holds an
`O_RDWR | O_NONBLOCK` peer open and confirms both implementations reject it as
a non-regular source, but no shared owner
API currently reproduces the no-peer blocking behavior. The CLI's negative
number option detection matches Python 3.14's `-\.?\d` prefix matcher, as
used by the pinned local oracle; older argparse versions may classify malformed
negative numerals differently. Parent substitution during hashing, concurrent
mutation and sudden owner death remain broader filesystem conformance gates.

## Evidence and judge gaps

Crate tests translate policy assertions from `tests/test_provider_exec.py`
(agy and protected instructions), `tests/test_cf_dispatch.py` (custody), and
the trusted_git policy exercised by provider confinement. Tests use explicit
identities, fixture clocks and bytes. No assertion sleeps or patched globals.

Python differential tests compare custody publication, bytes, digest, target
preservation, tamper refusal, parser/help diagnostics (including COLUMNS 40,
60 and 100, short and long argv[0], short-help clusters and negative-number
prefixes), FIFO refusal with a peer held open, fd duplicate offset behavior,
component-walk errors, OS-byte filenames, and missing-output diagnostics. The
shared oracle uses `PROVENANT_PORT_PYTHON` (default `python3`), `-I`, an explicit
environment, and Python 3.12+. Other differentials compare interface decisions,
trusted Git records/grants and error paths, lane revision diagnostics, and
non-UTF-8 quarantine index framing and caller order. Oracles print `skip:` when
unavailable unless `PROVENANT_PORT_DIFF_REQUIRED=1`. Custody comparisons omit
real device/inode values; source handles and OS identity policy have direct
filesystem tests.

Output custody has parser/help/error/exit differential coverage. Broader J1
command-owner entry-point and transcript coverage, J2 release/snapshot schema
and mutant coverage, and J3 caller transcripts remain cut-over gates for this
inert crate; they are not claimed here. Cross-family review is chair-owned. The fixed filename-byte defect is recorded as RP-2001 in divergences.md.
Snapshot views remain in-memory; their persisted wire encoding belongs to the
later record integration. No intended behaviour divergences are accepted; no Python/TypeScript source is deleted.

Lane rrc-3 route: `codex/gpt-6.1-sol@high` via Fabric. The lane source baseline
is `7021182d535f6c730c29713e77d5bca76893fd06`; main was merged through
`49a1cc3bceedcd0fb4c14183f6afff3ea01f6a84`. The safe filesystem FIFO timing gap
is proposed RP-RRC3-004, not an accepted change. RP-RRC3-005 records the current
COLUMNS/default-width observation and 4096-column cap; interactive terminal
geometry remains a command-owner integration gap. Unicode decimal conversion
also awaits a core API and is explicitly marked in pending. These gaps keep
output custody off the completed-unit list.
