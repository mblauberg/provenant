# Disk report

Owner: `crates/os/src/disk.rs` (measurement), `crates/confinement/src/disk.rs`
(lane classes), `crates/run/src/disk.rs` (run inventory), `src/disk.rs`
(composition), `src/gc_sweep.rs` (sweep custody),
`crates/run/src/build_dir_gc.rs` (Cargo targets),
`crates/run/src/retention.rs` (scratch)
Anchor: `crates/os/src/disk.rs`, `crates/run/src/disk.rs`, `src/gc_sweep.rs`

`provenant disk report` inventories lane caches, worktrees and the host state
root; `DiskReport` documents the fields and the [disk and GC runbook](../runbooks/gc.md)
the procedure. Paths are relative to `tools/front-door/`.

## Strictly read-only

The report creates, deletes, repairs or settles nothing; SQLite may touch only
an existing store's `-wal` and `-shm` companions. An unreadable run store falls
back to a filesystem-only inventory with an error.
- Why: inspecting disk use must be safe while lanes run.

## Bounded walks that never follow links

The walk never follows symlinks or crosses a device boundary, and each measured
path has an entry, time and depth budget; reaching one records an error and
keeps the bytes measured so far. Hard links count once.
- Why: a link or a huge tree must not send the report outside its roots or hang it.

## Figures are observations, not promises

No figure promises bytes recoverable by deletion, a fallback measurement says
so, and the report is a sequence of observations, not a snapshot.
- Why: users decide deletions from these numbers.

## Classes grant no authority

Lane classes reuse GC's owner and reason policy. Missing or ambiguous writer
evidence stays unknown and protects its cache. A `reclaim` hint never becomes
deletion authority: GC revalidates its own conditions.
- Why: a report row must never become deletion authority.

## Scratch and cache custody

Released read-only attempt scratch becomes collectable after `gc.idle`, without
branch, PR, graph or worktree conditions. Settled attempt `tmp/` scratch becomes
collectable after `gc.idle` once the attempt is terminal with all terminal
effects complete and no live holder, writer slot or Cargo lock holds the files;
live attempts are never touched. Busy or unreleased attempts remain protected, and collection
preserves durable results, handoffs, logs and process records.

Lanes steer build output to the shared project target (`lanes.cargo_target = "shared"`).
When a lane configures `CARGO_TARGET_DIR` under an attempt's `tmp/` directory,
launch and handoff output emit a warning. Discard archives exclude ignored build output
at creation (`target/` and any directory marked with `CACHEDIR.TAG`), and expire
after `gc.discard_archive_retention` (defaults to 14 days). In shared targets,
stale incremental session directories (`debug/incremental/*`) older than `gc.idle`
and not locked are trimmed; held or unprobeable locks report as busy and are spared.

`lanes.cargo_cache_max` covers both private lane and shared Cargo targets. It
accepts bytes or a percentage of the volume and defaults to 10% of the cache volume.
Its effective value and source are observable. Launch and disk-floor collection
reclaim eligible targets least recently used first; bounded previews report
partial measurements explicitly. Cargo build locks and live attempt custody
protect targets independently of size pressure. The newest seed generation and
recent writes retain their existing protection.

An EPERM process with complete descriptor evidence and no observed cwd or
descriptor beneath a candidate produces a warning once rather than making every
worktree unknown. Positive path evidence protects the affected candidate;
incomplete descriptor evidence and other unknown custody still prevent removal.
For worktree output, foreign policy uses verified worktree creation and lane
custody, not a configured path. A verified primary checkout and managed linked
worktrees are eligible regardless of foreign cleanup policy. A bound bare Git
owner may collect its linked worktrees; an unbound bare repository cannot.
Every output mutation holds writer exclusion and requires a fresh free census.
- Why: space recovery must retain evidence and live data while collecting idle
  read-only scratch and both forms of Cargo output.

## Bounded sweeps

Each sweep observes the overridable `gc.sweep_budget`, reports its value and
source, and records where it stopped so a later sweep continues there. Its
single-flight guard is released on every exit. A dead or over-budget holder is
detected and takeover emits a warning. Recursive surveys persist bounded
traversal progress and accumulated measurements so a large tree makes progress
across passes. A survey snapshot never grants removal authority: takeover and
resumption require fresh candidate custody checks.
- Why: a large or abandoned sweep must not prevent later collection.

Collection belongs to [worktrees](supervisor.md#worktrees) and
[state retention](state.md#retention-keeps-evidence).

## Dispatch and new-build pressure

Submission, admission and new builds take one decision, `DiskGate::decide`:
the thresholds from `GcSettings::disk_limits` plus the effective strength of
the declared `DISK_FLOOR` policy rule. Below `gc.free_floor`, warn and proceed.
Below `gc.critical_free` (default 2 GiB; `0B` disables it) `DISK_FLOOR` decides:
it warns and proceeds by default, for every lane class; `policy.DISK_FLOOR =
"refuse"` holds new work and `"off"` silences it. Messages report available
bytes, both thresholds, their configuration or shipped-default sources and the
`provenant gc` remedy. Running builds continue. The build shim receives the
host's serialised gate in `PROVENANT_BUILD_DISK_LIMITS` when `PortTable.build_admission`
selects builtin admission. The shim receives its selected backend and effective source
in `PROVENANT_BUILD_ADMISSION_BACKEND` and `PROVENANT_BUILD_ADMISSION_SOURCE`.
A command backend streams the repository wrapper around Cargo; `none` skips
build admission. Both retain the independent local-Cargo execution policy. Builtin sandbox-off preparation
takes the same decision against its target volume before launch. A record that
cannot be written still fails as a data-integrity error, outside this decision.
- Why: low space is not one of the four hard boundaries
  ([ADR 0035](../adr/0035-four-hard-boundaries-and-flexible-everything-else.md)); a collection target must not
  prevent ordinary lane work, and a host that wants a hard floor declares it.
