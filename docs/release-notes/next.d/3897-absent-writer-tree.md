## Fixes

### Lanes and settlement

- `provenant reconcile --abandon` can settle a lane whose writer checkout has already been removed: an absent worktree holds nothing and is free even when the census cannot decide its root. Any other metadata error, a dangling symlink or an existing path keeps the census rule. Part of #3897.
