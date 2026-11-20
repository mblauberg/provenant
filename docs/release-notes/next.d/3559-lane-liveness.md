## Fixes
### Lanes
- `status NAME --full` now reports a live lane's last observed event time, host-local attempt directory, masked current child command and known wait reason. Admission shims record a redacted, bounded argv when a lane waits for or holds a heavy-job slot, so the chair can tell a lane blocked in a repository lock from a hung writer. (#3559)
