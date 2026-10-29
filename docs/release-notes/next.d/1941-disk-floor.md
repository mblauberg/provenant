## Breaking changes
### Lanes and settlement

- **The `[disk_guard]` table and `provenant disk-guard` are removed.** Delete `[disk_guard]` from your configuration. Lanes now pause their own builds below `gc.free_floor` and resume at the same floor, housekeeping runs when a lane settles, and `install` retires the old disk-guard agent (#1941, #1952, #1965).
