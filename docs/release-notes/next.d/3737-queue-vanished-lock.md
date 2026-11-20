## Fixes

### Lanes and settlement

- Build admission no longer fails spuriously when another participant's queue
  lock or ticket disappears during a scan. A vanished entry is now treated as
  departed at every listing, stat and lock step; a waiter refuses a lock whose
  path was unlinked and replaced after it was taken, and recreates its own
  ticket lock if a reaper removes it before the flock lands. Real I/O failures
  still surface. (#3737)
