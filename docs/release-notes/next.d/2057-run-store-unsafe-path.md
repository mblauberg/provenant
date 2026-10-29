## Fixes
### Lanes and settlement

- `lanes --wait` no longer fails with a run-store path error when a running lane replaces its process checkpoint, and remaining run-store path errors name the path and the failed check (#2057, #2106).
