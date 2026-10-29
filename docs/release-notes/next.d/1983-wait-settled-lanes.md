## Fixes

- `lanes --wait` returns at once when every named lane has already settled (#1983).
- `lanes --wait` and `--watch` retry through transient run-store contention until their deadline, and time out only if the contention persists (#2013).
