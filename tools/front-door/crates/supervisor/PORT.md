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
| `legacy_lane` | Existing binary lane's signal, spawn, reactor and process-observation mechanisms, relocated for R7. S2 owns the lifecycle redesign. |
| Module root | Real-ID executable check, reentrant account lookup and process replacement for surviving binary owners. |

S2 retains observation-before-spawn, WNOWAIT/reserved identity, provider
StopReason and guardian handoff responsibilities. Census and liveness are not
reintroduced as S1 public diagnostic APIs.

## Design clarifications

Core's `WaitBudget` stores a monotonic deadline, so lock acquisition also needs
the corresponding `Clock`. It samples remaining budget once and derives a local
`Instant` deadline. The clock owner is passed explicitly; there is no global
clock coordinate or unbounded `None` path.

Direct-child custody requires default `SIGCHLD` without `SA_NOCLDWAIT` and no
independent host reaper. Every spawn queries and refuses an incompatible host
policy before creating a child. The process edge must keep that policy fixed
while children are owned; blocked `SIGCHLD` alone remains supported. This
preserves the unreaped PID reservation used for direct-child signalling.

Utility policy is supplied as a supervisor API for confinement's S3 facade.
No workflow-to-supervisor dependency is introduced.

Stable lock leaves use the reserved `.lock` suffix. Generic directory mutation
APIs refuse that suffix as source or destination, even through another anchor.
This protects stable inodes without a process-global registry. The trusted
owner still protects directory permissions from unmanaged filesystem actors.

`Clock::wall_now` returns a typed error: an infallible wall clock cannot report
an OS value outside Jiff/core's wire range without panicking or inventing time.
The legacy lane retains its existing start-display matching, implemented with
safe Jiff, until S2 removes that old-subject field. It grants no S1 authority.

PR #1350's private `sys::legacy_lane` keeps the previous provider subject for
its remaining Python importer. Its existing group destructor, observer wait,
signal-handler state and lifecycle policy are S2 work; moving them does not
claim the guardian redesign has landed. The new utility owner has none of
those destructor waits or signal-by-observed-PID interfaces.

Merging #1350 also required Rust 1.99 formatting and small mechanical clippy
changes in the stacked core/records dependency (chunk iteration and let chains).
These do not change its types or wire contracts.
