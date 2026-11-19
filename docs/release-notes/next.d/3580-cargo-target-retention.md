## Additions

### Lanes and settlement

- `provenant gc` now collects a private Cargo target nothing has written for `gc.cargo_target_idle` (default `30d`), whatever `lanes.cargo_cache_max` allows. This covers targets of worktrees the chair or a person made, which were never collected before. A target held by a Cargo lock or a live lane is always kept. The preview names each target with its worktree and reason (`idle`, `over budget` or `tombstone`), reports allocated bytes, and shows the budget and idle settings with their sources. (#3580)
- Retiring the old shared Cargo targets now honours `gc.sweep_budget` and the sweep lease. Each directory is renamed to a tombstone before it is deleted, and an interrupted pass resumes with the tombstones first. The preview probes and measures each directory as apply would, and every kept directory is reported with its cause: a held lock, a recent write or a failed lock probe. (#3576)
