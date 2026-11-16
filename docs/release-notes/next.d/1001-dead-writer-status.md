## Fixes

### CLI and install

- Status distinguishes exact live writers, reboot-confirmed unattended writers, pending settlement and unknown liveness. Same-boot absence stays unknown because a checkpoint can miss a later escaped child; use `provenant reconcile TARGET` for recovery, and retain the fence while evidence is unknown. (#1001)
- Worktree probes and cache guards keep durable writer reservations fenced when the kernel lock disappears, until run-owned reconciliation settles the attempt.
