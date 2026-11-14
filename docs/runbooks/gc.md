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

GC collects eligible provider state, payloads, build output, worktrees, refs and
old records. The [disk spec](../specs/disk.md) owns retention invariants; inspect
`provenant config schema` for settings. A failed upstream fetch preserves
worktrees. Record a ref's reported head before recreating it. Configure
regenerable and keep paths for each project before collecting output; unknown
ignored files can prevent removal. See [worktrees](../worktrees.md) for explicit
removal and [lane recovery](lane-lifecycle.md#recovery) for uncertain custody.

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
