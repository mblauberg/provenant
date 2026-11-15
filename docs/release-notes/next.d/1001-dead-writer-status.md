## Fixes

### CLI and install

- Status now distinguishes live, unattended, settling and unobservable writers using scoped native identity and a complete bounded host census; `provenant tick` queues only proven absent or settling attempts for recovery. Unknown evidence stays unknown and status remains read-only. (#1001)
- Worktree probes and cache guards keep durable writer reservations fenced when the kernel lock disappears, until run-owned reconciliation settles the attempt.
