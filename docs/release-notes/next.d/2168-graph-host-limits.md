## Behaviour changes

- Graphs use one host-configured limit set. Document limits are ignored with a
  warning, and paused graphs resume after the host limit is raised.
- Ignore the deprecated host configuration key `limit_ceiling` with a warning.
  Replace `--extend limits.*` with a change to the corresponding host limit
  before resuming.
