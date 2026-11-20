# Disk use and garbage collection
Owner: provenant
Anchor: tools/front-door/src/tick.rs

What `provenant disk report` and `provenant gc` do and the decisions around
them; the rules are in the [disk spec](../specs/disk.md) and every key is in
`provenant config schema --all` (`retention.*`, `gc.*`, `gc.projects`).

## Inspect, preview, apply

```sh
provenant --project /absolute/path/to/repo disk report --json   # read-only; check data.errors first
provenant --project /absolute/path/to/repo gc --dry-run --json  # preview everything
provenant --project /absolute/path/to/repo gc --apply --json
```

- Inspect report errors before relying on totals; a partial report is not a
  complete census.
- Preview cleanup before applying it. Busy items are skipped for a later pass.
  Apply takes fresh custody and census evidence; a preview does not authorise
  deletion. Inspect JSON for per-item outcomes.
- For free-space and scheduling settings, see `provenant config schema`.
- `gc --idle 6h` overrides the configured idle lifetime for this run; `--idle 0`
  makes ownerless idle artifacts eligible immediately.

## What is removed and when

GC executes the typed adapters in `RETENTION_CLASSES` for its trigger; full GC
includes ordinary GC classes. Pressure collection runs only adapters that declare
the pressure trigger, preserving its project-scoped scratch/payload pass without
ageing provider state or collecting checkpoints. Tick enters the same path.
Provider state, scratch and payload-cap policies run as separate scheduled
adapters. The declaration
also owns settings and subsystem ownership; adding a class requires collection
and measurement implementations. Preview and apply measure caches under the
sweep budget alone, never a fixed entry count, so one large cache cannot end the
pass before later classes run. A measurement the budget stops is marked partial
in JSON and text and resumes on the next pass; a timed-out process census keeps
custody unknown. A class the pass never reached reports `not run in this pass`
and an unavailable byte row, never a measured zero.
The preview uses local ref evidence without forge reads. Apply refreshes custody
before removal. Measurements do not follow links; APFS clones may share physical
blocks, so the change in volume free space can differ. Apply removes eligible attempt scratch without a separate
pre-deletion size walk. Partial observations and failed paths remain visible in
JSON and text. Byte rows come from each adapter; unavailable observations remain
explicit rather than appearing as zero. The compact apply summary reports one
`known bytes` figure, the sum of cache, attempt scratch, discard archive, Cargo
target and worktree bytes. It is a lower bound: classes without a measurement are
not counted.

`gc.idle` defaults to `6h` and accepts `0` or a duration in seconds, minutes,
hours, days or weeks. It applies to ownerless idle artifacts and standalone
caches. Settled attempt scratch becomes eligible after `gc.idle`, regardless of
its branch, PR, graph or worktree, once terminal effects and custody are complete.
Collection removes `tmp/` and candidate build targets while retaining results,
handoffs, logs and process records. Unreleased or busy attempts are never collected.

`gc.stale_graph_age` defaults to `1d` and uses the same duration syntax. A
paused or pending graph older than this age whose branch has merged or whose PR
is closed is reported as a warning with the age and its evidence. GC warns
rather than refuses: a preview never stops a graph. `gc --apply` requests a
stop for those graphs through the same cancellation path as `provenant stop`.
The effective age and its origin (shipped default or host configuration)
are shown in the warning; `provenant config show --full` shows the key's source.

After a merge, background cleanup targets only that branch's worktree and its
attempt scratch. It does not scan the run store or collect shared caches. At lane
launch, free space below `gc.free_floor` starts a best-effort background full
sweep. Each sweep has an overridable `gc.sweep_budget` (default `5m`; `0`
checkpoints immediately). Diagnostics show its value and source, and a stopped
sweep records its position
for the next pass.
The single-flight guard is released on exit. Dead or over-budget holders are
detected and takeover is reported as a warning.

`lanes.cargo_cache_max` budgets private lane and shared Cargo targets together.
It accepts bytes or a percentage of the cache volume, defaults to 10% of that
volume and shows its effective source in diagnostics. For example, set
`"lanes.cargo_cache_max" = "50G"` or `"10%"` in the host configuration
`[knobs]` table; profile and launch overrides use the same knob. Launch evicts eligible targets in
least-recently-used order when the cache exceeds the budget. Full GC also
removes, whatever the budget allows, a managed target whose worktree is gone or
was replaced by another directory at its path once the build-directory grace has
passed, one unused for `gc.cargo_target_idle` (default `30d`, same duration
syntax), including targets of chair-made and hand-made worktrees, and any
tombstone an interrupted removal left. A target's last use is the newer of its
`.provenant-last-used` stamp, written at each launch and by
`worktree seed-target`, and its newest file time; `target-lint` shares its
cache's worktree. Just before removing a target or a lane cache, `--apply`
rechecks that last use with the Cargo locks held; a launch or seed that
stamped it meanwhile keeps it, and a later pass decides again. `gc --dry-run` shows measured allocated bytes against the
budget with the budget and idle settings and their sources, and names each
target it would remove with its worktree, reason (`worktree gone`,
`worktree replaced`, `idle`, `over budget`, `tombstone`) and the evidence
that dated its last use (`last-use stamp` or `file times`); it names what
`--apply` would remove from the same state, and incomplete previews say so. Allocated bytes include blocks shared with APFS
clones, so the volume may gain less.
Targets of the removed `lanes.cargo_target = "shared"` mode under
`~/Library/Caches/provenant-shared-cargo/` count toward both the budget and the
free-space sweep until full GC retires them. A preview probes and measures each
project directory as apply would; `--apply` renames each whose Cargo locks are
free and that was not written within the build-directory grace to a tombstone,
then deletes it under `gc.sweep_budget`. The next pass deletes tombstones
first without measuring them and resumes a stopped measurement where it ended,
so a directory larger than one pass still goes. Each kept directory is listed with its cause: a held lock, a recent write
or a probe failure. Live attempt custody and held Cargo build
locks always protect a target. Full GC also protects recent writes and the
newest Cargo seed generation for each project. Private lane caches older than `gc.idle` are
eligible when their worktree has gone or was replaced; a cache whose worktree
exists outside the project is kept until `gc.cargo_target_idle`. Unknown
custody keeps a cache. Provenant writes Cargo's `CACHEDIR.TAG` into each lane
cache and seed generation target it creates, so backup and indexing tools skip
them; GC never reads the tag.

`gc.free_floor` is an advisory collection target. Below it, and below
`gc.critical_free` (2 GiB by default), dispatch, admission and new builds warn
and continue; diagnostics show the effective thresholds and sources. A host that
wants a hard floor sets the `policy.DISK_FLOOR` knob to `refuse`. Settlement
releases private Cargo output without waiting for worktree removal; attempt
read-only scratch follows the idle rule above. Registry and Git downloads live
in the host CARGO_HOME and are owned by Cargo,
not lane retention. Lanes fetch into those stores by default; with
`domains.fabric.lanes.cargo_source_writes = false` they are read-only and a missing
dependency requires a host fetch.

Cleanup separates **subsumed** (the exact tip is retained elsewhere, including
verified tree equivalence) from **landed** (integration-target or forge merge
proof). Failed forge reads use last-fetched refs for subsumption and never imply
an absent PR. Source refs and candidate tips are rechecked under mutation custody.

Foreign worktrees and local branches are included by default. Their first pass
previews and records that preview for the repository incarnation. Configure
`gc.projects.<project>.foreign_worktrees = 'keep'` to opt out, `keep_paths` for
path/ref patterns, and `integration_branches` for integration-ref patterns.
Worktree creation records distinguish Provenant-created checkouts from foreign
ones independently of their location. Older checkouts without creation or lane
custody evidence remain foreign. Declared ignored output in a verified
primary or managed worktree remains eligible when foreign cleanup is disabled;
writer custody and the process census still protect live work. A verified bound
bare repository can collect its managed linked worktrees without a primary
checkout. An unbound bare repository cannot grant GC authority.
A process denied observation with EPERM produces one aggregated warning with a
count and its meaning (custody is unknown for those processes) rather than
blocking every candidate or printing a line per process, when descriptor
inspection is complete and no observed path is beneath a candidate. Add `--full`
to list each affected PID. Observed cwd or descriptor use beneath a candidate
protects that candidate. Incomplete descriptor evidence and other unknown custody
remain protective.
Idle clean worktrees with no PR, or a closed unmerged PR at exactly the current
tip, retire without deleting their branch. Unknown custody, run, forge or census
feeds keep the affected item. Initialised submodules are checked recursively. Removal requires clean working
data and a retained head in a scoped Git store that survives deletion; detached
unpushed heads, stores inside removed metadata and unknown child custody keep
the parent worktree.

Remote pruning defaults to pushed or held refs in the ref ledger; set
`remote_branches = 'never'` to disable it. `remote_min_tip_age_days` defaults to
one day. Every remote deletion rechecks open PR heads and bases and leases the
push on the observed tip. Unobserved remote branches are outside this surface.

Explicit discard first publishes a private state-root archive: exact-tip Git
bundle, binary working/index patches, NUL-delimited status, untracked and ignored
files in a no-follow ustar, and a manifest. Unrepresentable paths or changing
source data keep the original worktree. `gc.discard_archive_retention` controls archive
retention (14 days by default). Clean subsumed removals need no archive.

Record a ref's reported head before recreating it. Configure regenerable and keep
paths before collecting output; undeclared ignored files such as local databases
prevent removal. See [worktrees](../worktrees.md) and
[lane recovery](lane-lifecycle.md#recovery) for uncertain custody.

Port command receipts live under `port-receipts/<owner>/` as versioned records.
Admission receipts stay until the attempt is terminal and its release is proven.
Naming and post-create receipts declare a 30-day default age in the record.
Release proof is removed after its sibling receipts so an interrupted pass can
resume. Receipt byte observations count record lengths; their reclaimed bytes
are reported separately. A receipt whose publication is visible but whose directory sync is unconfirmed is reported as
visible evidence with a durability warning. Unknown or unreadable records are
reported and retained.

Graph checkpoint GC also resumes interrupted collection: under the existing
graph lock, a reserved name with no checkpoint permits removal of remaining
prompt blobs, sidecars and pending markers. The name reservation remains so a
collected identity is never reused. A busy graph is skipped for a later pass.

On macOS the tick agent schedules collection; other installs schedule
`provenant tick` themselves. A sweep that fails or is interrupted is retried
within 15 minutes, then after 1 hour, 4 hours and 16 hours; tick shows
`gc=retry_pending` with the failure's age and next retry until it completes, and
the daily deadline moves only on completion. A sweep that stopped at its
budget ends as `partial`, which counts as progress and resumes next sweep. Inspect the tick logs when a sweep
fails.

## Detached services and unsettled lanes

Provider executables do not inherit the guardian's writer, custody or port
leases. Ordinary detached services may keep running without holding those lane
descriptors. Unsandboxed services have no Seatbelt membership mark; an open
resource path alone never authorises killing them.

If cleanup reaches its stop deadline, warnings name an observable PID, short
command and writer-lock resource, or state that the holder is unknown. Close
an identified service through its normal controls, or wait for it to exit, then
run `provenant reconcile NAME/NODE#N`. Descriptor discovery is bounded and may
be incomplete. A remaining unresolved reservation is a custody fence: do not
unlink `writer.lock` or delete the worktree to bypass it. Guardian loss retains
that fence even when no process holds the kernel lock.
