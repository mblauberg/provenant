## Breaking changes
### Lanes and settlement

- **`gc.free_floor` now governs all lane admission and build pauses, and the `[disk_guard]` table is removed.** Read-only lanes use the same floor as writers. Delete `[disk_guard]` from your configuration (#1941).
- **`provenant disk-guard` is removed.** Active lanes watch disk pressure themselves, and scheduled GC runs daily as a backstop (#1941).
