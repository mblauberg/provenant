## Additions

### Lanes and settlement

- `provenant gc` now collects a private Cargo target nothing has used for `gc.cargo_target_idle` (default `30d`), whatever `lanes.cargo_cache_max` allows. This covers targets of worktrees the chair or a person made, which were never collected before. A target whose worktree is gone, or was replaced by another directory at the same path, goes once the build-directory grace has passed. A lane cache whose worktree exists outside the project is kept only until `gc.cargo_target_idle`. A target held by a Cargo lock, a live lane or unobserved custody is always kept. (#3580)
- A target's last use is now the newer of its `.provenant-last-used` stamp and its newest file time. Launches and `provenant worktree seed-target` write the stamp, so a freshly seeded target no longer looks idle because of the file times its clone carried. (#3580)
- The preview names each target with its worktree, its reason (`worktree gone`, `worktree replaced`, `idle`, `over budget` or `tombstone`) and which evidence dated its last use. It reports allocated bytes and shows the budget and idle settings with their sources. (#3580)
- Lane caches and seed generation targets now carry Cargo's `CACHEDIR.TAG`, so backup and indexing tools skip them. (#3580)
- Retiring the old shared Cargo targets now honours `gc.sweep_budget` and the sweep lease. Each directory is renamed to a tombstone before it is deleted. Tombstones are deleted without being measured, and a stopped measurement resumes on the next pass, so a directory larger than one pass still goes. The preview probes and measures each directory as apply would. Every kept directory is reported with its cause: a held lock, a recent write or a failed lock probe. (#3576)
