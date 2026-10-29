## Additions
### Pipelines and graphs
`pr wait`, `land` and the graph CI gate report two new states, `Conflict` (the PR conflicts with its base, so CI never starts) and `NotScheduled` (no check registered for the head). Each carries a `next:` line. Required checks on the exact head stay mandatory.
