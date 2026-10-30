## Fixes

- Stop a supervisor from retrying exit publication forever when its attempt directory is removed between publishing `process-exit.json` and syncing the directory, including when the sync reports a durability failure rather than a missing path. A gone attempt also skips terminal settlement, which has nowhere left to publish.
