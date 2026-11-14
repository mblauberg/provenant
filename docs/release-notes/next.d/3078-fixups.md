## Fixes

### Lanes and settlement

- Complete handoffs survive host token-budget stops as successful settlements, retaining the host cause. Dispatch and new builds warn below `gc.free_floor` and stop only below `gc.critical_free`, which defaults to 2 GiB. (#3078)

### CLI and install

- `wait --timeout` clamps oversized budgets to `domains.wait.max_timeout_seconds`, with the effective limit and source shown. (#3078)
