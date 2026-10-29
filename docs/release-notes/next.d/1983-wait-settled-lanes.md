## Fixes
### Lanes and settlement

- `lanes --wait` returns at once when every named lane has already settled (#1983).
- `lanes --wait` and `--watch` retry through transient run-store contention and time out only if it persists (#2013).
