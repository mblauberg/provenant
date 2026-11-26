## Fixes

### Pipelines and graphs

- Graph tasks now carry their admitted `PinnedSubject` onto the run request, so a dispatched attempt stores the same base, head, origin and policy that admission pinned. Status and later peer head checks read that persisted subject instead of re-resolving a mutable ref. (#4021)
