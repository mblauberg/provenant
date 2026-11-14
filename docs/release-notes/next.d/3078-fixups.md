## Breaking changes

### Lanes and settlement

- Replace `policy.LAUNCH_DISK`, `policy.ADMISSION_DISK` and `policy.BUILD_DISK` overrides with `gc.free_floor` and `gc.critical_free`; set `gc.critical_free='0B'` to disable the critical stop. (#3078)

## Fixes

### Lanes and settlement

- Complete handoffs survive host token-budget stops as successful settlements, retaining the host cause. Dispatch and new builds warn below `gc.free_floor` and stop only below `gc.critical_free`, which defaults to 2 GiB. (#3078)

### CLI and install

- `wait --timeout` clamps oversized budgets to `domains.wait.max_timeout_seconds`, with the effective limit and source shown. (#3078)
