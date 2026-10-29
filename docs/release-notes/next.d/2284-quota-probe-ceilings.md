## Additions
### Accounts, quota and routing

- `[domains.quota]` now accepts `max_parallel` up to 16 and `wall_budget_seconds` up to 600, so a host can raise the probe batch size and wall budget above the unchanged defaults of 3 and 55 (#2284).
