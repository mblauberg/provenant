## Fixes
### Lanes and settlement

- A batch slice whose `integrate:<branch>` delivery completed now settles as
  succeeded and starts the slices that wait on it with `after`. The run status
  view re-derives the delivery receipt from `delivery.json` even when its head
  is the integrated tip rather than the sealed writer head (which still names
  the pre-integration graph base), and the delivery settlement effect is
  marked done from that same receipt. Previously the pre-integration
  `not_attempted` snapshot was kept, the slice settled `other_failure`, and the
  terminal-effects delivery stayed pending with its worktree retained. A
  conflicted integration still reports blocked with the conflicting files.
  (#3846)
