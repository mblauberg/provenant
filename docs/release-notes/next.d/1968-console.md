## Additions
### Chair, inbox and console

- `console projects`, `console sessions`, `console overview`, `console board` and `console tree` show local projects, chair sessions, pending requests, runs and nested runs. Pages hold 50 rows by default and 100 at most, and session discovery stops at 16,384 entries and sets `scan_truncated` (#1968).
- `console board` and `console tree` take `--hosts` to add peer observations. Local results are kept, and the output names peers that are unreachable, incomplete or do not support the read (#1968).
- The menu's console overview refreshes when its records change and keeps the last good view if a refresh fails (#1968).
