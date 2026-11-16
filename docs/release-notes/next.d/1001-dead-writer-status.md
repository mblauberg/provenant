## Fixes

### CLI and install

- Status now distinguishes live, unattended, settling and unobservable writers using scoped native identity and a complete bounded host census. Use `provenant reconcile TARGET` to queue recovery for a verified unattended writer; unknown evidence stays fenced, and status remains read-only. (#1001)
- Worktree probes and cache guards keep durable writer reservations fenced when the kernel lock disappears, until run-owned reconciliation settles the attempt.
