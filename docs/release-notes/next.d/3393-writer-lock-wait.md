## Fixes

### Fabric

- Concurrent writer launches queue on the writer registry lock rather than failing immediately with `writer_lock_busy`. (#3393)
- Graph-target retries of a single-node graph retry that node instead of silently reprinting status. (#3393)
