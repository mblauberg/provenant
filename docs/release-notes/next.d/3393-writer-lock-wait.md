## Fixes

### Fabric

- Writer launches retry repository lock contention with bounded, cancellable backoff after releasing custody. Queued attempts retain their identity and can be retried after the launch budget expires; running attempts must settle first. (#3393)
- Graph-target retries of a single-node graph retry that node instead of silently reprinting status. (#3393)
- Writer-binding I/O diagnostics retain bounded operation and OS error metadata without exposing private paths or error messages. (#3393)
- Ending a wait, including timeout and `--any` completion, leaves scheduler-owned launch preparation running; queued observations retain their next retry wake and original launch deadline. (#3393)
