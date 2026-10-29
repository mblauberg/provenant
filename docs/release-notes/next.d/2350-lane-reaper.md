## Fixes
### Lanes and settlement

- Settle running lanes whose guardian died, and cancelled lanes that never launched, on the next `provenant lanes` read instead of leaving them `running` or `starting`. A live guardian or writer lock is never taken. `lanes --json` rows now carry `overdue` (past the runtime deadline plus grace) and `stale` (no live guardian or provider). (#2350)
