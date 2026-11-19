## Fixes
### Lanes and settlement

Settlement now waits a bounded time for a writer worktree's lock and wakes when
it is released. Lock contention no longer counts as a recovery failure, so a
busy holder can no longer block settlement and leave a steered writer's node
running with no successor. Status names the holder: `waiting for writer lock
<path>, held by attempt <id> (guardian live, reserved 4s ago)`. A steered
writer's continuation waits for custody release instead of being dropped, and
`provenant stop` on such a node resolves its queued steering at once, cause
shown. Commands that settle a lane's cleanup no longer wait on another holder's
lock; they leave it to the settlement worker. A lock held for three minutes
with no live guardian and no live settlement owner is reported as blocked and
names the recovery commands; an unidentified holder keeps the wait visible.
Reconcile now shows a recorded settlement failure as past evidence with its age
and recording process (`recorded 45s ago by process <pid>: ...`), never as a
current failure. `provenant reconcile --abandon` now releases a node whose
guardian died with its cleanup unresolved once a fresh census proves every
recorded process gone and no process carries the lane's sandbox mark;
otherwise it refuses and names the live PID or the omitted descendants.
Deferred Git cleanup is now retried for launched writers too; a refused import
keeps its Git evidence instead.

Nodes blocked by lock contention before this release keep their blocked state.
Run `provenant reconcile <lane>`, or a bare `provenant reconcile` to sweep
every blocked node; an explicit reconcile resets the failure count and
settlement resumes. An import that settlement already sealed as refused
(status: `settlement exhausted three attempts; private Git retained for host
recovery`) is final and reconcile will not retry it. Release that node with
`provenant reconcile --abandon <lane> --reason <text>`: abandonment keeps the
private Git evidence but stops automatic import, so recover that work by hand.
