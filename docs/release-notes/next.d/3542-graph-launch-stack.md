## Fixes

### Pipelines and graphs

- Graph execution uses smaller stack frames in a debug build. The scheduling pass, gate evaluation, attempt admission and observation recording, the host launch preparation and the submit path are split into separate functions, and large values on the submit path are boxed. Threads that run graph execution now take one named size, `GRAPH_STACK_BYTES`, in place of the scattered `RUST_MIN_STACK` and 8 MiB worker overrides. (#3542)
