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

## What is removed and when

GC dispatches from `RETENTION_CLASSES`. Caches and build profiles are considered
before worktrees. Preview and apply use allocated tree bytes without following
links; APFS clones may share physical blocks, so the change in volume free space
can differ. Partial observations and failed paths remain visible in JSON and text.

`gc.free_floor` is an advisory collection target. New builds warn below it and
stop only below `gc.critical_free` (2 GiB by default); diagnostics show the effective
threshold and source. Settlement releases private Cargo output and attempt scratch
without waiting for worktree removal. Registry and Git downloads remain shared.

Cleanup separates **subsumed** (the exact tip is retained elsewhere, including
verified tree equivalence) from **landed** (integration-target or forge merge
proof). Failed forge reads use last-fetched refs for subsumption and never imply
an absent PR. Source refs and candidate tips are rechecked under mutation custody.

Foreign worktrees and local branches are included by default. Their first pass
previews and records that preview for the repository incarnation. Configure
`gc.projects.<project>.foreign_worktrees = 'keep'` to opt out, `keep_paths` for
path/ref patterns, and `integration_branches` for integration-ref patterns.
Idle clean worktrees with no PR, or a closed unmerged PR at exactly the current
tip, retire without deleting their branch. Unknown custody, run, forge or census
feeds keep the affected item. Initialised submodules retain their independent
Git custody and working data.

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
