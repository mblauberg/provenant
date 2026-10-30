## Fixes

- Avoid retrying recovery while a recorded supervisor or guardian is stuck in uninterruptible sleep, and report the host stall in lane status. Workers stalled before their identity is recorded retain the existing queue expiry behavior.
