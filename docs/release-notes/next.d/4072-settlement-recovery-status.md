## Fixes

### Run settlement

- A lane whose settlement retries have stopped now reports why and names both `provenant reconcile <lane>` and `provenant reconcile --abandon <lane> --reason <text>` in its status, instead of leaving work looking automatically pending with no way to clear it. A writer that has exited is reported as having settlement recovery outstanding, without promising a retry an exhausted budget will never make. (#4072)

### Session and hooks

- The session-start inventory keeps a custody answer a child wrote before its deadline, even when that child is killed before it can exit. Bounded, paged reads now yield a partial summary that names an unavailable section and carries the next-page command rather than dropping a completed custody section to `unavailable: timed out`. (#4072)
