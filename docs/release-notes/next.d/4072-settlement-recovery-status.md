## Fixes

### Run settlement

- A lane whose settlement retries have stopped now reports why and names both `provenant reconcile <lane>` and `provenant reconcile --abandon <lane> --reason <text>` in its status, instead of leaving work looking automatically pending with no way to clear it. A writer that has exited is reported as having settlement recovery outstanding, without promising a retry an exhausted budget will never make. (#4072)

### Session and hooks

- Session-start custody and lane inventories stream completed rows before their deadline and offer a continuation for any unread rows. Lane pages use ordered index scans and defer process observation until needed, so a large history no longer needs a project-wide sort before it can report progress. (#4072)
