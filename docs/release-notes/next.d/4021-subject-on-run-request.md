## Fixes

### Pipelines and graphs

- Graph tasks now carry their admitted `PinnedSubject` on the graph document, checkpoint and coordination task, so a dispatched attempt uses the base, head, origin and policy that admission pinned. A peer placement records that subject as its own graph's subject; the run request carries only `head`, the one commit a lane reads. Status and later peer head checks read the persisted subject instead of re-resolving a mutable ref. (#4021)
