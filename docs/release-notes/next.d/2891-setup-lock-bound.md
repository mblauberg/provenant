## Additions
### Lanes and settlement

- Writers queue for the repository Git setup lock in arrival order. `domains.fabric.lanes.setup_lock_wait_seconds` sets the wait bound from 1 to 14400 seconds; the default is 14400. A timeout reports `SETUP_LOCK_TIMEOUT` and how to retry or adjust the bound (#2891).
