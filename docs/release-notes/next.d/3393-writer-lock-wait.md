## Fixes

### Fabric

- Writer launches retry repository lock contention with bounded, cancellable backoff after releasing custody. Queued attempts retain their identity and can be retried after the launch budget expires; running attempts must settle first. (#3393)
- Graph-target retries of a single-node graph retry that node instead of silently reprinting status. (#3393)
