## Fixes

### Writers and worktrees

- A pre-lease writer reservation whose launch never produced a holder is now discovered and recovered by `provenant reconcile` and applied GC, including when no attempt row exists; uncertain custody remains fenced and attempt evidence is retained. It is no longer reported as unresolvable legacy evidence asking for a previous Provenant version. (#3949, #3844)
