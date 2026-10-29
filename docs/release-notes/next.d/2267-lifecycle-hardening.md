## Fixes
### Lanes and settlement

- `provenant run --worktree-branch` now validates the prompt, title, route, fallback, knobs, delivery and access before it creates or claims the worktree. Admission uses the base pinned at creation, including relative expressions such as `HEAD^`; a failed preflight leaves no stray worktree or branch (#2267).
- Pruning a merged, clean worktree can exclude an unreadable foreign process when its credentials and the tree's permissions rule out write access. A denied same-user process, or a tree writable by group or others, remains unknown; cleanup still needs proven ownership, no live writer, a clean tree and the merged head (#2267).
- Each store transaction now gets its own contention allowance, so a long step between writes no longer expires the writes that follow it. Cancellation is unchanged (#2267).
