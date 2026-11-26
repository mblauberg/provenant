## Fixes

### Lanes and settlement

- A lane that committed and pushed before declaring a stop no longer settles as `stopped_no_work`. Terminal settlement reads one sealed evidence record and decides once from the launch-pinned input head and the writer's verified head; a remote-tracking ref, a later prose line or a restart cannot reclassify the attempt. The typed cause now agrees across `status`, graph state and telemetry. (#4020)
- Graph admission reserves the worst-case configured `on_stop` defaults in the checkpoint envelope, so a graph with full host/project defaults cannot be refused by a bound that undercounts them. (#4020)
- Host and project `on_stop` defaults now resolve from their documented registry families, including concrete stop causes and project names. (#4020)
