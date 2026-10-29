## Fixes
### Lanes and settlement

- `provenant run --worktree-branch` now validates the prompt, route, delivery and access before it creates the worktree, and records the base that creation pinned. A failed preflight no longer leaves a stray worktree or branch behind (#2267).
- Pruning a merged, clean worktree is no longer blocked by one process on the host that cannot be read. Cleanup still needs proven ownership, no live writer, a clean tree and the merged head, and an unknown target still keeps the tree (#2267).
- Each store transaction now gets its own contention allowance, so a long step between writes no longer expires the writes that follow it. Cancellation is unchanged (#2267).
