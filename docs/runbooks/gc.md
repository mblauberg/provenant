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
and measurement implementations. Preview bounds cache measurement and may omit
idle candidates whose recent writes could not be checked within the budget.
Its byte estimates can be partial; a timed-out process census keeps custody unknown.
Cargo cache previews are bounded and mark incomplete measurements in JSON.
The preview uses local ref evidence without forge reads. Apply takes a fresh full observation. Measurements do not follow links; APFS clones may share physical blocks, so the change in volume
free space can differ. Apply removes eligible attempt scratch without a separate
pre-deletion size walk. Partial observations and failed paths remain visible in
JSON and text. Byte rows come from each adapter; unavailable observations remain
explicit rather than appearing as zero.

`gc.idle` defaults to `6h` and accepts `0` or a duration in seconds, minutes,
hours, days or weeks. It applies to ownerless idle artifacts and standalone
caches. A released read-only attempt becomes eligible for scratch collection
after `gc.idle`, regardless of its branch, PR, graph or worktree. Writer scratch
remains eligible immediately when its work is finished: its branch is merged or
its PR closed, its graph is cancelled or superseded, or its worktree is removed.
Collection removes `tmp/` and candidate build targets while retaining results,
handoffs, logs and process records. Unreleased or busy attempts are never collected.

After a merge, background cleanup targets only that branch's worktree and its
attempt scratch. It does not scan the run store or collect shared caches. At lane
launch, free space below `gc.free_floor` starts a best-effort background full
sweep. Each sweep has an overridable `gc.sweep_budget` (default `5m`);
diagnostics show its value and source, and a stopped sweep records its position
for the next pass.
The single-flight guard is released on exit. Dead or over-budget holders are
detected and takeover is reported as a warning.

`lanes.cargo_cache_max` budgets private lane and shared Cargo targets together.
It accepts bytes or a percentage of the cache volume, defaults to 10% of that
volume and shows its effective source in diagnostics. For example, set
`"lanes.cargo_cache_max" = "50G"` or `"10%"` in the host configuration
`[knobs]` table; profile and launch overrides use the same knob. Launch evicts eligible targets in
least-recently-used order when the cache exceeds the budget. `gc --dry-run`
shows measured cache size against that budget; incomplete previews say so.
Shared targets under `~/Library/Caches/provenant-shared-cargo/` participate in
both the budget and free-space sweep. Live attempt custody and held Cargo build
locks always protect a target. Full GC also protects recent writes and the
newest Cargo seed generation. Private lane caches older than `gc.idle` are
eligible when their owner has settled or disappeared. Unknown custody keeps a cache.

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
A process denied observation with EPERM produces one warning rather than
blocking every candidate when descriptor inspection is complete and no observed
path is beneath a candidate. Observed cwd or descriptor use beneath a candidate
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
source data keep the original worktree. `gc.discard_archive_days` controls archive
retention (90 days by default). Clean subsumed removals need no archive.

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
`provenant tick` themselves. Inspect the tick logs when a sweep fails.

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
