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

Writer lanes build in the worktree's own private target (`lanes.cargo_target`,
[run spec](run.md#cargo-target-directory)); read-only lanes build in their
attempt scratch. When a writer's `CARGO_TARGET_DIR` is under an attempt's
`tmp/` directory, launch emits a warning. Discard archives exclude ignored build output
at creation (`target/` and any directory marked with `CACHEDIR.TAG`), and expire
after `gc.discard_archive_retention` (defaults to 14 days). In shared targets,
stale incremental session directories (`debug/incremental/*`) older than `gc.idle`
and not locked are trimmed; held or unprobeable locks report as busy and are spared.

`lanes.cargo_cache_max` covers both private lane and shared Cargo targets. It
accepts bytes or a percentage of the volume and defaults to 10% of the cache volume.
Its effective value and source are observable. Launch and disk-floor collection
reclaim eligible targets least recently used first. Full GC also removes,
whatever the budget allows and whoever made its worktree, a managed target whose
worktree is gone or replaced after the build-directory grace, and one unused for
`gc.cargo_target_idle` (default 30 days). One owner classification (live,
foreign, replaced or gone) serves both lane caches and targets; a foreign cache
waits for `gc.cargo_target_idle`. Last use is the newer of the launch and seed
stamp and the newest file time, and every GC removal names its target, owning
worktree, reason and the evidence that dated it. Every removal of a target or
a lane cache rechecks its last use with the Cargo locks held, immediately before
it detaches anything: the newest file time under those locks, the stamps, and
the cache root's activity marker, which each launch and seed replaces after its
stamp. Any use newer than the cutoff, or a marker that changed, keeps it for a
later pass. A preview names exactly what
apply removes from the same state. Measurement stops only at the sweep
deadline, preview included; a stopped census reports partial measurements
explicitly and resumes. Cargo build locks and live attempt custody protect
targets independently of size and age, and a failed lock probe keeps the target
with its cause reported. The newest seed generation and recent writes retain
their existing protection. Managed targets are renamed to a tombstone
under their held locks before deletion, so an interrupted pass never leaves a
partial target where Cargo builds; a tombstone is deleted unmeasured and a
stopped measurement resumes from the sweep cursor, so each pass makes progress.
Lane caches and seed generation targets carry Cargo's `CACHEDIR.TAG`; no
removal depends on it.

- Why: worktrees removed without a lane settlement, and chair or hand-made
  worktrees, otherwise keep their targets forever while the cache stays under
  budget; a fixed measurement cap starved every class after the first large
  cache; a seeded target's cloned file times made it look idle at once.

`provenant gc` reports a Cargo target's reclaimable bytes as private bytes --
the blocks no other clone shares -- where the filesystem can report them (APFS
private size). Seeded worktree targets are clones: their allocated figure counts
blocks shared with the seed that removal does not free, so private bytes keep
the preview from overstating reclaimable space. The measure is taken inside the
target census walk at each entry, so it adds no separate full-tree pass, stays
within `gc.sweep_budget` and resumes with the census cursor. Where the
filesystem cannot report private bytes, the report says so, reports allocated
bytes instead and never reports zero. The same figure feeds the budget
arithmetic and the per-target reclaimable totals; the report names which figure
it carries. A launch-time seed reservation is charged in that same figure: near
zero private bytes under a private-byte census, the seed's recorded allocated
size under the allocated fallback, so admission and the census account for the
incoming target in one unit. The budget compares that sum with the configured
byte cap, not volume occupancy: private bytes exclude blocks shared with a seed, so the sum
can stay below the cap while shared blocks still occupy disk. Eviction stays
least-recently-used; the byte figure does not rank targets by bytes freed.
- Why: a clone's allocated bytes are not the space its deletion recovers, and
  an unmeasured or zero fallback would mislead the same decision.

An EPERM process with complete descriptor evidence and no observed cwd or
descriptor beneath a candidate produces one aggregate warning with a count and
its meaning (custody is unknown for those processes), rather than making every
worktree unknown or printing one line per process. Per-PID detail stays behind
the gc `--full` flag. Positive path evidence protects the affected candidate;
incomplete descriptor evidence and other unknown custody still prevent removal.
For worktree output, foreign policy uses verified worktree creation and lane
custody, not a configured path. A verified primary checkout and managed linked
worktrees are eligible regardless of foreign cleanup policy. A bound bare Git
owner may collect its linked worktrees; an unbound bare repository cannot.
Every output mutation holds writer exclusion and requires a fresh free census.
`gc` names stranded work in its report: a registered or project worktree with
uncommitted edits and no live holder is kept and listed with its branch, linked
issue or PR, changed file and line counts and newest-edit age, so a lane that
stopped or died cannot lose its edits silently during cleanup. This is
reporting only; the ordinary dirty-work and custody guards still decide whether
a checkout may be removed, and an unknown holder is never called stranded.
- Why: space recovery must retain evidence and live data while collecting idle
  read-only scratch and both forms of Cargo output.

## Bounded sweeps

Each sweep observes the overridable `gc.sweep_budget`, reports its value and
source, and records where it stopped so a later sweep continues there. Its
single-flight guard is released on every exit. A dead or over-budget holder is
detected and takeover emits a warning. Only writers of the lease record take its
short metadata mutex; the holder's fence watcher reads the atomically replaced
record without it, so the watcher never delays the holder's own checkpoint and a
read that races a replacement is retried rather than read as a takeover.
Recursive surveys persist bounded
traversal progress and accumulated measurements so a large tree makes progress
across passes. A survey snapshot never grants removal authority: takeover and
resumption require fresh candidate custody checks. Each trigger and scope keeps
its own bounded restart record, so unfinished scopes remain independently
resumable without growing the single-flight lease record. An ambiguous legacy
inline cursor restarts its scope from the beginning rather than skipping work.
- Why: a large or abandoned sweep must not prevent later collection.

Collection belongs to [worktrees](supervisor.md#worktrees) and
[state retention](state.md#retention-keeps-evidence).

## Dispatch and new-build pressure

Submission, admission and new builds take one decision, `DiskGate::decide`:
the thresholds from `GcSettings::disk_limits` plus the effective strength of
the declared `DISK_FLOOR` policy rule. `gc.free_floor` defaults to 1% of the
state volume, capped at 5 GiB. Below `gc.free_floor`, warn and proceed.
Below `gc.critical_free` (default 2 GiB; `0B` disables it) `DISK_FLOOR` decides:
it warns and proceeds by default, for every lane class; `policy.DISK_FLOOR =
"refuse"` holds new work and `"off"` silences it. Messages report available
bytes, both thresholds, their configuration or shipped-default sources and the
`provenant gc` remedy. Running builds continue. Lane build shims receive the
frozen build configuration in `PROVENANT_BUILD_CONTEXT`; the build strategy
uses the existing admission limits and keeps the independent local-Cargo
execution policy. Builtin sandbox-off preparation takes the same decision
against its target volume before launch. A record that
cannot be written still fails as a data-integrity error, outside this decision.
- Why: low space is not a hard boundary
  ([ADR 0035](../adr/0035-three-hard-boundaries-and-flexible-everything-else.md)); a collection target must not
  prevent ordinary lane work, and a host that wants a hard floor declares it.
