## Additions
### Lanes and settlement

- `domains.fabric.lanes.setup_lock_wait_seconds` sets the time a writer waits for its repository's Git setup lock, from 1 to 14400 seconds; the default is 120. A timeout reports `SETUP_LOCK_TIMEOUT` and how to retry or adjust the bound (#2891).
