## Breaking changes

### Lanes and settlement

- Replace `policy.LAUNCH_DISK`, `policy.ADMISSION_DISK` and `policy.BUILD_DISK` with one `policy.DISK_FLOOR` rule over `gc.free_floor` and `gc.critical_free`. Low space warns by default for every lane class; set `policy.DISK_FLOOR='refuse'` for a hard floor. (#3078)

## Fixes

### Lanes and settlement

- Complete handoffs survive host token-budget stops as successful settlements, retaining the host cause. Submission, admission and new builds share one low-space decision: they warn below `gc.free_floor` and `gc.critical_free` (2 GiB by default) and refuse only when `policy.DISK_FLOOR` is set to `refuse`. (#3078)

### CLI and install

- The configuration reference lists `gc.projects.<project>.foreign_worktrees` as `keep` or `collect` with default `collect`, matching what the runtime accepts; every enumerated key now takes its values and default from the owning declaration. (#3078)
- `wait --timeout` clamps oversized budgets to `domains.wait.max_timeout_seconds`, with the effective limit and source shown. (#3078)
