## Fixes

### Lanes and settlement

- Writer binding refusals name the holding attempt, branch, worktree, reservation age and liveness. `reconcile` reports the stale writer custody it releases, and stored-record errors identify the file and record type. Process exits written by Provenant 2.0.0 remain readable. (#3533)
