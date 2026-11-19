## Fixes
### Lanes and settlement

Settlement now waits a bounded time for a writer worktree's lock and wakes when
it is released. Lock contention no longer counts as a recovery failure, so a
busy holder can no longer block settlement and leave a steered writer's node
running with no successor. Status names the holder: `waiting for writer lock
<path>, held by attempt <id> (guardian live, reserved 4s ago)`. A steered
writer's continuation waits for custody release instead of being dropped, and
`provenant stop` on such a node resolves its queued steering at once, cause
shown.
