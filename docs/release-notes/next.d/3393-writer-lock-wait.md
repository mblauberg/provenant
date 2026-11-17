## Fixes

### Fabric

- Writer launches retry repository lock contention with bounded, cancellable backoff after releasing custody. Queued attempts retain their identity and can be retried after the launch budget expires; running attempts must settle first. (#3393)
- Graph-target retries of a single-node graph retry that node instead of silently reprinting status. (#3393)
- Writer-binding I/O diagnostics retain bounded operation and OS error metadata without exposing private paths or error messages. (#3393)
- Wait and tick advance due launch retries inline. If a wait times out or completes with `--any` during preparation, it finishes that bounded step before returning. Observation never cancels work; explicit graph cancellation reaches preparation before provider launch. Queued attempts retain their wake and original launch deadline. (#3393)
- Repository confinement refusals keep their typed error code, and a zero provider-fence wait no longer expires repository preparation. (#3393)
