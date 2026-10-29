## Breaking changes

- **`gc.free_floor` now governs all lane admission and build pauses, and the `[disk_guard]` table is removed.** Read-only lanes use the same floor as writers. Delete `[disk_guard]` from existing configuration.
- **`provenant disk-guard` is removed.** Active lanes supervise disk pressure, and scheduled GC provides a daily backstop.
