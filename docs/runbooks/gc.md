# Disk use and garbage collection
Owner: provenant
Anchor: tools/front-door/src/tick.rs

Procedure for `provenant disk report` and `provenant gc`. The rules are in the
[disk spec](../specs/disk.md) and [state spec](../specs/state.md#retention-keeps-evidence).
Keys, defaults and their sources are in `provenant config schema --all`
(`gc.*`, `retention.*`, `lanes.cargo_cache_max`) and `provenant config show --full`.
A lane cache that holds only its ownership marker and `CACHEDIR.TAG` is
reclaimed in one pass; lane launches no longer create one.

## Inspect, preview, apply

```sh
provenant --project /absolute/path/to/repo disk report --json   # read-only; check data.errors first
provenant --project /absolute/path/to/repo gc --dry-run --json  # preview everything
provenant --project /absolute/path/to/repo gc --apply --json
```

- A report with errors is a partial census; do not rely on its totals.
- A preview never authorises deletion: apply refreshes custody and census
  evidence, and skips busy items for a later pass. Read the JSON for per-item
  outcomes.
- `gc --idle DURATION` overrides `gc.idle` for one run; `--idle 0` makes
  ownerless idle artefacts eligible at once. `--full` lists each affected PID
  where the text output aggregates them.

## What is removed and when

`RETENTION_CLASSES` in `crates/config/src/gc.rs` declares each class, its
trigger and its settings. Full GC runs every class; pressure collection (free
space below `gc.free_floor`) runs only classes that declare the pressure
trigger; `provenant tick` takes the same path.

- Preview reads local refs only, with no forge reads. Failed forge reads fall
  back to last-fetched refs and never imply an absent PR.
- A class the pass did not reach reports `not run in this pass`; a measurement
  stopped by `gc.sweep_budget` is `partial` and resumes next pass. Neither is a
  measured zero. `known bytes` in the apply summary is a lower bound, because
  classes without a measurement are not counted.
- A paused or pending graph older than `gc.stale_graph_age` whose branch has
  merged or whose PR is closed warns with its age and evidence. `gc --apply`
  stops it through the path `provenant stop` uses.
- After a merge, background cleanup removes only that branch's worktree and
  attempt scratch. At lane launch and at settlement, free space on the state
  volume below `gc.free_floor` starts one bounded best-effort sweep; no daemon
  is required. The effective floor and its source are printed by `provenant gc`
  (`free-space floor` in text, `free_floor_bytes`/`free_floor_source` in JSON).
  Below `gc.critical_free`, work warns and continues unless `policy.DISK_FLOOR`
  is `refuse` (see the
  [disk spec](../specs/disk.md#dispatch-and-new-build-pressure)).
- `provenant gc --free-floor SIZE` waits for a sweep already holding the lease
  for up to `gc.sweep_budget` and then collects on the freed lock with the
  remaining budget. It never reports an empty skip as success; if the bound
  expires it fails and says so.
- The report's `uncollected` rows (text: `uncollected ...`) name what stayed,
  its bytes (or that bytes are unknown), and why: live holder, uncertain
  custody, retained evidence, unmerged work and similar. `gc --json` and
  `gc --full` show one row per candidate; the default text view aggregates
  them by class and reason.
- Cargo targets are measured in private bytes (blocks no other clone shares),
  and eviction drops the least recently used first. `gc --dry-run` names each target it
  would remove with its worktree, reason (`worktree gone`, `worktree replaced`,
  `idle`, `over budget`, `tombstone`) and the evidence that dated its last use
  (`last-use stamp` or `file times`). Live custody and held Cargo locks always
  protect a target. Custody and budget rules are in the
  [disk spec](../specs/disk.md#scratch-and-cache-custody).
- Registry and Git downloads live in the host `CARGO_HOME`, which Cargo owns.
  Lanes write to them by default; with `domains.fabric.lanes.cargo_source_writes` set to `false`
  a missing dependency needs a host fetch.

### Project bindings

- A registered project whose common directory is gone protects nothing. Launch
  confinement, tick settlement and forge polling skip its binding, and GC
  retires the binding once its record has been idle for `gc.idle` (or the
  `--idle` override); `gc --dry-run` names the bindings it would retire. The
  private host-project directory stays, so a checkout that returns reuses its
  ports and settings.
- A registered project that still exists but whose layout cannot be read warns
  at launch and protects that project's whole root instead of refusing an
  unrelated launch.

### Worktrees and branches

- Foreign worktrees and local branches are collected by default. The first pass
  previews and records that preview for the repository. Opt out with
  `gc.projects.{project}.foreign_worktrees` set to `keep`; `keep_paths` and
  `integration_branches` take path and ref patterns.
- Removal needs clean working data and a head retained in a Git store that
  survives deletion. Unknown custody, run, forge or census evidence keeps the
  item, and initialised submodules are checked recursively.
- Undeclared ignored files, such as local databases, keep a worktree. Declare
  regenerable and keep paths before collecting output, and record a ref's
  reported head before recreating it.
- Remote pruning acts on pushed or held refs (`remote_branches = 'managed'`
  by default; `never` disables it). Each deletion rechecks open PR heads and
  bases and leases the push on the observed tip.
- Explicit discard first publishes a private archive (retention
  `gc.discard_archive_retention`); if the source changes during archiving or a
  path cannot be represented, the worktree stays. Clean subsumed removals need
  no archive.
- A process denied observation (EPERM) yields one aggregated warning, not a
  block, when no observed path is beneath a candidate and its credentials could
  own that candidate; other users' and system processes are not reported.
  Incomplete descriptor evidence still protects the candidate.

### Scheduling

On macOS the tick agent schedules collection; other installs schedule
`provenant tick`. A failed or interrupted sweep retries after 15 minutes, then
1, 4 and 16 hours (capped at the daily cadence); tick shows `gc=retry_pending`
with the failure's age and next retry. A sweep that stopped at its budget ends
as `partial` and counts as progress. Read the tick logs when a sweep fails.

Port command receipts and graph checkpoints have their own classes. A
collected graph name stays reserved so its identity is never reused, and an
unreadable receipt is reported and kept. For uncertain custody see
[worktrees](../worktrees.md) and [lane recovery](lane-lifecycle.md#recovery).

## Detached services and unsettled lanes

Provider executables do not inherit the guardian's writer, custody or port
leases, and an open resource path alone never authorises killing a service.

If cleanup reaches its stop deadline, warnings name an observable PID, short
command and writer-lock resource, or say the holder is unknown. Close an
identified service through its own controls, or wait for it to exit, then run
`provenant reconcile NAME/NODE#N`. Descriptor discovery is bounded and may be
incomplete. An unresolved reservation is a custody fence: never unlink
`writer.lock` or delete the worktree to bypass it. Guardian loss keeps the fence
even when no process holds the kernel lock.
