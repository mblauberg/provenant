## Breaking changes

`gc.free_floor` now governs all lane admission and build pauses. Read-only
lanes use the same admission floor as writers. The `[disk_guard]` table and
its independent thresholds and process list have been removed; delete that
table from existing configuration.
