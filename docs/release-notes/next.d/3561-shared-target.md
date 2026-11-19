## Fixes
### Lanes and settlement
- Stop worktrees serving each other stale Cargo artifacts. `lanes.cargo_target` now defaults to `private`: each worktree gets its own target in its lane cache, cloned warm from the repository's host seed with every dep-info aged, so workspace members rebuild once from the worktree's sources while registry crates stay fresh. The `shared` value, which gave every worktree of a repository one target, is removed: Cargo maps each workspace member to the same units in every worktree and judges them fresh by source mtimes, so a worktree with older sources reused another branch's artifacts and reported symbols missing that exist in its source. `provenant config check` reports a `shared` setting and the default applies; `provenant gc` lists the old `provenant-shared-cargo` directories and `--apply` removes those not in use. Where the filesystem cannot clone, the target stays unseeded and the host warns once. `project` remains for the worktree's own `target/`. (#3561)

## Features
### CLI and install
- Add `provenant worktree seed-target PATH`, which seeds a checkout's private Cargo target from the host seed and prints it for `CARGO_TARGET_DIR`, so a chair or person can build warm outside a lane. (#3561)
