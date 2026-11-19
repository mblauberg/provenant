## Fixes

### Lanes and settlement

- A lane guardian no longer loses a SIGTERM, cancellation or process event that arrives while it is cleaning up or retaining custody, so a signalled guardian ends within a bound and releases the lane's writer lock. Before, a guardian could ignore SIGTERM and hold the lock for hours. Retained custody is now bounded by `domains.run.retained_custody_seconds` (default 3600). Past the limit the guardian records the unresolved processes and what blocked them on the exit, then exits with status `timeout` (exit 124), leaving the fence for `provenant reconcile`. A signalled guardian also stops retrying an undurable reservation or exit publication after 5 s. `provenant status` names the holding guardian's PID and age while it is alive, and says when it is gone. (#3585)
