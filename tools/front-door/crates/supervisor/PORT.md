# S1: Safe OS owner

Rulebook: PORTING.md version 7. Design: `docs/specs/rust-port/design/supervisor.md`.
Author route: `codex/gpt-6.1-sol@high` via Fabric.

The prior runtime-parity API and its tests are deleted. Python and TypeScript
leaves remain with their old subjects until the design's last-importer gates
(R4+R5 and the named CLI/Fabric owners). S1 adds no bridge or public verb.

## Syscall wrapper inventory

| Owner under `sys` | Native operations and boundary |
| --- | --- |
| `child` | Preallocated argv/environment and pointer arrays, strict exec, signal-mask/disposition reset, descriptor CLOEXEC sweep. Post-fork callbacks use async-signal-safe calls. |
| `raw_exec` | Owned direct-child spawn, nonblocking pipe flags, direct-child signals and bounded poll. No numeric-PID signal API is exported. |
| `reaper` | Reserved fixed-capacity slots, fair `try_wait` polling, transfer on error/drop. No destructor signal or child wait. |
| `pid` | Linux CWD link and Darwin `proc_pidinfo`, typed absence/permission/unsupported and malformed-record failures. CWD grants no custody. |
| `fs` | Held-path open/stat/mkdir/unlink/symlink, atomic rename modes, entropy, errno, directory iteration and nonblocking flock. File ownership transfers once. |
| `lane_files` | Existing lane's held-path creation and no-replace link publication, pending S2's owner replacement. |
| `lane` | Existing binary lane's signal, spawn, reactor and process-observation mechanisms, relocated for R7. S2 owns the lifecycle redesign. |
| Module root | Real-ID executable check, reentrant account lookup and process replacement for surviving binary owners. |

S2 retains observation-before-spawn, WNOWAIT/reserved identity, provider
StopReason and guardian handoff responsibilities. Census and liveness are not
reintroduced as S1 public diagnostic APIs.

## Design clarifications

Core's `WaitBudget` stores a monotonic deadline, so lock acquisition also needs
the corresponding `Clock`. It samples remaining budget once and derives a local
`Instant` deadline. The clock owner is passed explicitly; there is no global
clock coordinate or unbounded `None` path.

Utility policy is supplied as a supervisor API for confinement's S3 facade.
No workflow-to-supervisor dependency is introduced.
