## Fixes
### Lanes and settlement

- A batch slice whose `integrate:<branch>` delivery completed now settles as
  succeeded and starts the slices that wait on it with `after`. The run status
  view re-derives the delivery receipt from `delivery.json` even when its head
  differs from the sealed writer head, and the delivery settlement effect is
  marked done from that same receipt. Previously the pre-integration
  `not_attempted` snapshot was kept, the slice settled `other_failure`, and the
  terminal-effects delivery stayed pending with its worktree retained. The
  head-mismatch allowance is scoped to integrate deliveries: a `push` or `pr`
  receipt must still name the sealed head, a receipt for another branch or
  remote is ignored, and a failed receipt stays failed. A conflicted
  integration still reports blocked with the conflicting files. Runs already
  stuck in this state under v2.8.2 still need `provenant retry`; this change
  does not re-derive already-settled failed nodes on reconcile. (#3846)
