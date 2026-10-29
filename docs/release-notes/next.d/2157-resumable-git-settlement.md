## Behaviour changes

- Resume durable import, publication, recording and release phases after a crash.
  Release host build capacity at provider exit and keep cancel and reconcile
  responsive while Git runs without the worktree writer lock.
- Deduplicate background settlement workers and expire unknown native owners
  after ten minutes so recovery can continue.
