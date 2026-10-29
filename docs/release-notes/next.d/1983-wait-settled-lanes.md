## Fixes
### Lanes and settlement

- `lanes --wait` returns at once when every named lane has already settled, and `lanes --wait` and `--watch` retry through transient run-store contention (#1983, #2013).
