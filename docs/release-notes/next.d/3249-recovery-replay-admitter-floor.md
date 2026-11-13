## Fixes
### Fabric and execution

- Graph recovery replay now restores `parent_sandboxed` and carries `admitted_by` when reconstructing an accepted attempt specification after a host crash, matching the sealed request digest and preventing false `OperationConflict` errors (#3249).
