# Disk report

Owner: `crates/os/src/disk.rs` (measurement), `crates/confinement/src/disk.rs`
(lane classes), `crates/confinement/src/worktrees/census.rs` (EPERM census),
`crates/run/src/disk.rs` (run inventory), `src/disk.rs`
(composition), `src/gc_sweep.rs` (sweep custody),
`crates/run/src/build_dir_gc.rs` (Cargo targets),
`crates/run/src/retention.rs` (scratch)
Anchor: `crates/os/src/disk.rs`, `crates/run/src/disk.rs`, `src/gc_sweep.rs`

`provenant disk report` inventories leftover lane caches, worktrees and the host state
root; `DiskReport` documents the fields and the [disk and GC runbook](../runbooks/gc.md)
the procedure. Paths are relative to `tools/front-door/`.

The state root also holds `warnings.sqlite3`, one STRICT `seen` table that remembers
per-change warning keys (code, first and last seen time, count). Rows older than the
host setting `warnings.dedup_retention_days` (default 30 days, shown with its source in
`provenant config schema --all`, or with `provenant config show`) are pruned on each write. The retired `config-warning-*`
and `root-warning-*` marker files are deleted by the `retire-warning-markers` install
migration; nothing else in the state root is touched.
- Why: dedup memory is bounded and survives a restart, without leaving a marker per
  finding in the state root.

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
writer slot, live settlement owner or Cargo lock holds the files, and sooner when
its branch is merged, its PR closed, its graph cancelled or superseded or its
worktree removed. A settlement driver keeps the attempt's private Git objects and
review source checkout busy until its import and final reconciliation finish;
GC holds the same lease throughout collection and retains scratch until settlement is
complete, including between retries or when ownership cannot be determined. Existing
custody failures remain reported when they prevent collection. Live, busy and
unreleased attempts are never touched, and collection preserves durable results,
handoffs, logs and process records. After a merge, cleanup targets only
the merged branch's worktree and attempt scratch.

Three size caps bound the classes that grow without a retention age.
`lanes.cargo_cache_max` budgets the lane Cargo cache, `retention.payload_cap_bytes`
budgets bulk payload across attempt directories, and `gc.attempt_scratch_max`
(an overridable default whose value and source appear in GC output) caps one
attempt's scratch. `0` disables the scratch cap. An attempt whose scratch is
measured over that cap becomes collectible once settled, without waiting out
`gc.idle`; a bounded observation that cannot measure the whole tree keeps the
scratch rather than assuming it is over cap. Live, continuing and unsettled
attempts are never collected by a cap.
- Why: a single large settled attempt must not hold the volume hostage until
  its idle age passes, while uncertain measurement must never authorise removal.

Lanes do not build. Cargo targets here belong to the chair's warm check, CI or
a person's worktree; the host never allocates one per lane. A warm slot target
is keyed by `build.target` (default `toolchain,worktree`): `toolchain` uses the
compiler selected by Cargo, including a `+toolchain` argument, and `worktree`
sets Cargo's workspace-only rustc wrapper to a checkout-specific executable
path. Workspace-member artifacts stay isolated while dependency artifacts
remain in the shared toolchain target. Discard archives
exclude ignored build output (`target/` and
any `CACHEDIR.TAG` directory) and expire after `gc.discard_archive_retention`.
In shared targets, unlocked `debug/incremental/*` sessions older than `gc.idle`
are trimmed; held or unprobeable locks report busy and are spared.

`lanes.cargo_cache_max` budgets shared Cargo targets; its effective value and
source are observable. Explicit GC and disk-floor collection reclaim eligible
targets least recently used first. Full GC also removes, within or beyond the budget and
whoever made the worktree, a managed target whose worktree is gone or replaced
and one unused for `gc.cargo_target_idle`. Lane launches no longer create
private Cargo caches; a leftover directory holding only its ownership marker and
`CACHEDIR.TAG` is reclaimed in one pass whatever its owning project. One owner
classification (live, foreign, replaced, gone) serves the leftover lane caches
and shared targets.
Last use is the newer of the recorded stamp and the newest file time,
and every removal names its target, owning worktree, reason and dating
evidence. Each removal rechecks last use under the Cargo locks immediately before
detaching anything (newest file time, stamps and the cache root's activity
marker); newer use keeps it for a later pass. Cargo locks and live attempt
custody protect targets whatever their size or age, a failed lock probe keeps
the target with its cause, and recent writes stay protected. A managed target
is renamed to a tombstone under its held
locks before deletion, so an interrupted pass never leaves a partial target
where Cargo builds. A preview names exactly what apply removes from the same
state. Measurement stops only at the sweep deadline, preview included; a
stopped census reports partial measurements and resumes. Nothing depends on
`CACHEDIR.TAG`.
- Why: worktrees removed without a lane settlement, and chair or hand-made
  worktrees, would otherwise keep their targets forever while the cache stays
  under budget.

`provenant gc` reports a Cargo target's reclaimable bytes as private bytes (the
blocks no other clone shares) where the filesystem can report them. Where
private bytes are unavailable the report says so, reports allocated bytes and
never zero, and names which figure it carries; the budget arithmetic and
per-target totals use that same figure.
The budget compares the sum with the configured byte cap, not volume occupancy,
and eviction stays oldest-use-first rather than ranking by bytes freed.
- Why: allocated bytes are not the space a deletion recovers, and a zero
  fallback would mislead the same decision.

An EPERM process with complete descriptor evidence and no cwd or descriptor
beneath a candidate yields one aggregate warning with a count, not one line per
process or an unknown worktree; per-PID detail is behind `gc --full`. Only a
process that could own a candidate counts towards that warning: kernel
credentials that differ from the current user and every root owner put other
users and system daemons outside the cohort, and missing credentials keep a
process in it. No warning is printed when the cohort is empty. Positive
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
restarts its scope from the beginning rather than skipping work. Attempt
scratch collection precedes the broader provider-state and payload-cap scans so
they cannot consume its first pass budget. The advisory dweller census runs
after the collection classes: one bounded observation is cached for the whole
pass and reused for every settled attempt
and terminal worktree instead of re-walking the process table per candidate.
A census cut short by its bound or the sweep deadline is reported incomplete;
an undecided candidate is kept, never treated as free. The report names how
many settled candidates the pass decided, collected and deferred, with the
known bytes reclaimed and the bytes still retained, or unknown when retained
bytes were not measured. Deferred paths and byte observations refer to
`attempt-scratch/<attempt-id>/tmp`; custody locks remain with the attempt record.
A preview labels reclaimable bytes and candidates it
would collect; a class not reached is reported as unmeasured. Partially removed
bytes count as reclaimed, never as retained.
- Why: a large or abandoned sweep must not prevent later collection, and a
  slow census must never spend the budget that collection needs.

Collection belongs to [worktrees](supervisor.md#worktrees) and
[state retention](state.md#retention-keeps-evidence).

## Floor-triggered and joined sweeps

`gc.free_floor` is overridable and its effective value and source (compiled
default, host setting or config file) are reported by `provenant gc` in both
human and JSON output. Lane launch already starts a bounded sweep when free
space on the state volume is below the floor; settlement does the same through
the cleanup worker, so no daemon is needed. Above the floor, or with
`gc.schedule = false`, a checkpoint starts nothing.
- Why: a host that fills up between daily ticks still recovers space, while a
  healthy host is never swept merely because a lane started.

An explicit `provenant gc --free-floor SIZE` that finds the single-flight
lease held joins that sweep for up to the configured `gc.sweep_budget`, then
runs its own collection on the freed lock with the budget the join left over.
It never turns a busy lease into a successful empty skip: if the bound expires
it fails with the wait and the fact that this invocation collected nothing.
The lease record does not carry a holder name, so neither does the failure.
- Why: the operator who asks for space back must see either a real result or a
  failure, not "sweep already running; skipped" with zero bytes.

## What was not collected

Every gc report lists the candidates it deliberately kept: path, class,
observed bytes (or an explicit unknown), and the reason — live holder,
uncertain custody, retained evidence, unmerged work, held Cargo lock, recent
write and similar. Bytes already measured for attempt scratch and Cargo
targets are carried into those rows; where only a class total was measured,
the row reports that total. A measurement that cannot report bytes says so
rather than claiming zero. JSON and `gc --full` give one row per candidate;
the default human view aggregates by class and reason so the lines survive its
compact clip.
- Why: collection must preserve evidence and live data; naming what stayed is
  what lets an operator act on it.

## Dispatch and new-build pressure

Submission, admission and new builds take one decision, `DiskGate::decide`: the
thresholds from `GcSettings::disk_limits` plus the strength of the declared
`DISK_FLOOR` policy rule. `gc.free_floor` defaults to 1% of the state volume,
capped at 5 GiB. Below `gc.free_floor` it warns and proceeds. Below
`gc.critical_free` `DISK_FLOOR` decides: it warns and proceeds by default for
every lane class, `policy.DISK_FLOOR = "refuse"` holds new work and `"off"`
silences it. Messages report available bytes, both thresholds, their sources and
the `provenant gc` remedy. Running builds continue, and builtin sandbox-off
preparation takes the same decision against its target volume. A record that
cannot be written still fails as a data-integrity error, outside this decision.
- Why: low space is not a hard boundary
  ([ADR 0035](../adr/0035-three-hard-boundaries-and-flexible-everything-else.md)),
  and a host that wants a hard floor declares it.
