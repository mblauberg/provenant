## Fixes

### Writers and worktrees

- A pre-lease writer reservation whose launch never produced a holder is now listed as stale evidence that `provenant reconcile` and GC can clear, with a remedy this version can carry out. It is no longer reported as unresolvable legacy evidence asking for a previous Provenant version. (#3949, #3844)
