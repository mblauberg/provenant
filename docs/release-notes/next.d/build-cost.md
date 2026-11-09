## New

### Lanes

- `lanes.local_cargo` (`all`, `check` or `none`) limits which cargo commands a lane may run locally. The lane's `cargo` shim enforces it for every provider: a refused command exits 1 with one line saying CI runs it and which setting changes this. Set it under `[knobs]`, per run with `--knob lanes.local_cargo=check`, or per graph node with `attempt.local_cargo`. The default is `all`.

## Fixes

### Lanes

- Writer lanes on Linux now seed their Cargo target, registry and Git store with `FICLONE` reflinks on btrfs or XFS, as macOS already did, so a new worktree no longer recompiles third-party dependencies. Where the filesystem cannot clone, the lane builds from scratch and the host prints a warning.
- The prompt every writer lane receives no longer tells it to start with T0 and choose T1 targets. It now says to run one cheap compile check and leave the tiers to CI, which stops lanes queuing `clippy` and `nextest` builds the repository's CI already runs.
