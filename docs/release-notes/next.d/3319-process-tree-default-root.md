## Fixes

### CLI and install

- When `PROVENANT_PROCESS_TREE` is set, `provenant process-tree --json` returns the supervised command's sampled subtree without requiring `--root`. Pass `--root PID` to select a subtree from that sample, as described in the [consumer contract](../../specs/consumer-contract.md#process-tree). (#3319)
