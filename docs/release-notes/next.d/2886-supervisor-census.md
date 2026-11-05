## Fixes
### Supervision

- Supervisors sleep until process events or the next deadline while a lane runs, reducing unnecessary wakeups (#2886).
