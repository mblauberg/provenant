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

The report creates, deletes, repairs or settles nothing; SQLite may touch only an
existing store's `-wal` and `-shm` companions. An unreadable run store falls back
to a filesystem-only inventory with an error.
- Why: inspecting disk use must be safe while lanes run.

## Bounded walks that never follow links

The walk never follows symlinks or crosses a device boundary, and each measured
path has an entry, time and depth budget; reaching one records an error and keeps
the bytes measured so far. Hard links count once.
- Why: a link or a huge tree must not send the report outside its roots or hang it.

## Figures are observations, not promises

No figure promises bytes recoverable by deletion, a fallback measurement says so,
and the report is a sequence of observations, not a snapshot.
- Why: users decide deletions from these numbers.

## Classes grant no authority

Lane classes reuse GC's owner and reason policy. Missing or ambiguous writer
evidence stays unknown and protects its cache. A `reclaim` hint never becomes
deletion authority: GC revalidates its own conditions.
- Why: a report row must never become deletion authority.

## Scratch and cache custody

Released read-only attempt scratch becomes collectable after `gc.idle`, with no
branch, PR, graph or worktree condition. Settled writer scratch (`tmp/`) does too
once the attempt is terminal with its effects complete and no live holder,
writer slot or Cargo lock holds the files, and sooner when its branch is merged,
its PR closed, its graph cancelled or superseded or its worktree removed. Live,
busy and unreleased attempts are never touched, and collection preserves durable
results, handoffs, logs and process records. After a merge, cleanup targets only
the merged branch's worktree and attempt scratch.

Writer lanes build in the worktree's own private target (`lanes.cargo_target`,
[run spec](run.md#cargo-target-directory)); read-only lanes build in attempt
scratch, and launch warns when a writer's `CARGO_TARGET_DIR` is under an
attempt's `tmp/`. Discard archives exclude ignored build output (`target/` and
any `CACHEDIR.TAG` directory) and expire after `gc.discard_archive_retention`.
In shared targets, unlocked `debug/incremental/*` sessions older than `gc.idle`
are trimmed; held or unprobeable locks report busy and are spared.

`lanes.cargo_cache_max` budgets private lane and shared Cargo targets together;
its effective value and source are observable. Launch and disk-floor collection
reclaim eligible targets least recently used first. Full GC also removes, within
or beyond the budget and whoever made the worktree, a managed target whose
worktree is gone or replaced and one unused for `gc.cargo_target_idle`. One owner
classification (live, foreign, replaced, gone) serves lane caches and targets.
Last use is the newer of the launch and seed stamp and the newest file time,
and every removal names its target, owning worktree, reason and dating
evidence. Each removal rechecks last use under the Cargo locks immediately before
detaching anything (newest file time, stamps and the cache root's activity
marker); newer use keeps it for a later pass. Cargo locks and live attempt
custody protect targets whatever their size or age, a failed lock probe keeps
the target with its cause, and the newest seed generation and recent writes
stay protected. A retired shared target is renamed to a tombstone under its held
locks before deletion, so an interrupted pass never leaves a partial target
where Cargo builds. A preview names exactly what apply removes from the same
state. Measurement stops only at the sweep deadline, preview included; a
stopped census reports partial measurements and resumes. Nothing depends on
`CACHEDIR.TAG`.
- Why: worktrees removed without a lane settlement, and chair or hand-made
  worktrees, would otherwise keep their targets forever while the cache stays
  under budget.

`provenant gc` reports a Cargo target's reclaimable bytes as private bytes (the
blocks no other clone shares) where the filesystem can report them. Seeded
targets are clones, so their allocated figure counts blocks removal does not
free. Where private bytes are unavailable the report says so, reports allocated
bytes and never zero, and names which figure it carries; the budget arithmetic,
per-target totals and a launch-time seed reservation all use that same figure.
The budget compares the sum with the configured byte cap, not volume occupancy,
and eviction stays oldest-use-first rather than ranking by bytes freed.
- Why: a clone's allocated bytes are not the space its deletion recovers, and a
  zero fallback would mislead the same decision.

An EPERM process with complete descriptor evidence and no cwd or descriptor
beneath a candidate yields one aggregate warning with a count, not one line per
process or an unknown worktree; per-PID detail is behind `gc --full`. Positive
path evidence protects the candidate, and incomplete descriptor evidence or other
unknown custody still prevents removal. Foreign policy uses verified worktree
creation and lane custody, not a configured path: a verified primary checkout and
managed linked worktrees are eligible whatever the foreign policy, a bound bare
Git owner may collect its linked worktrees and an unbound bare repository may
not. Every output mutation holds writer exclusion and needs a fresh free census.
`gc` also lists stranded work: a worktree with uncommitted edits and no live
holder is kept and reported with its branch, linked issue or PR, change counts
and newest-edit age. An unknown holder is never called stranded, and the
ordinary dirty-work and custody guards still decide removal.
- Why: space recovery must keep evidence and live data while collecting idle
  scratch and both forms of Cargo output.

## Bounded sweeps

Each sweep observes the overridable `gc.sweep_budget`, reports its value and
source, and records where it stopped so a later sweep continues there. Its
single-flight guard is released on every exit, and a dead or over-budget holder
is detected with a warning on takeover. Only writers of the lease record take its
short metadata mutex; the holder's fence watcher reads the atomically replaced
record without it, so a read that races a replacement is retried, not read as a
takeover. Recursive surveys persist bounded traversal progress and measurements
so a large tree progresses across passes. A survey snapshot never grants removal
authority: takeover and resumption need fresh candidate custody checks. Each
trigger and scope keeps its own bounded restart record, so unfinished scopes
resume independently without growing the lease record, and an ambiguous cursor
restarts its scope from the beginning rather than skipping work.
- Why: a large or abandoned sweep must not prevent later collection.

Collection belongs to [worktrees](supervisor.md#worktrees) and
[state retention](state.md#retention-keeps-evidence).

## Dispatch and new-build pressure

Submission, admission and new builds take one decision, `DiskGate::decide`: the
thresholds from `GcSettings::disk_limits` plus the strength of the declared
`DISK_FLOOR` policy rule. Below `gc.free_floor` it warns and proceeds. Below
`gc.critical_free` `DISK_FLOOR` decides: it warns and proceeds by default for
every lane class, `policy.DISK_FLOOR = "refuse"` holds new work and `"off"`
silences it. Messages report available bytes, both thresholds, their sources and
the `provenant gc` remedy. Running builds continue, and builtin sandbox-off
preparation takes the same decision against its target volume. A record that
cannot be written still fails as a data-integrity error, outside this decision.
- Why: low space is not a hard boundary
  ([ADR 0035](../adr/0035-three-hard-boundaries-and-flexible-everything-else.md)),
  and a host that wants a hard floor declares it.
