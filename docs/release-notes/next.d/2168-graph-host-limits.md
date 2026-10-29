## Behaviour changes

- Graphs use one host-configured limit set. Document limits are ignored with a
  warning, and paused graphs resume after the host limit is raised.
- Remove `limit_ceiling` from host configuration. Replace `--extend limits.*`
  with a change to the corresponding host limit before resuming.
