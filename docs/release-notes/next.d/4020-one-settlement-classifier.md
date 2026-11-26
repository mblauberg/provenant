## Fixes

### Lanes and settlement

- A lane that committed and pushed before declaring a stop no longer settles as `stopped_no_work`. Terminal settlement reads one sealed evidence record and decides once from the launch-pinned input head and the writer's verified head; a remote-tracking ref, a later prose line or a restart cannot reclassify the attempt. The typed cause now agrees across `status`, graph state and telemetry. (#4020)
