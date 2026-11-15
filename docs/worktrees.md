# Worktrees
Owner: confinement worktree lifecycle and supervisor custody
Anchor: [`lifecycle.rs`](../tools/front-door/crates/confinement/src/worktrees/lifecycle.rs)

Use linked worktrees for implementation and keep the primary checkout on the
integration branch. Use `<primary-repository-root>/.worktrees/<branch-with-slashes-as-dashes>`;
do not nest worktrees.

## Helper

```sh
provenant worktree create fix/repo-example --base origin/main
provenant worktree list
provenant worktree remove fix-repo-example
```

See `provenant worktree --help` for options and
[configuration](runbooks/configuration.md#worktree-setup) for project setup.
If a worktree is refused, inspect it with `provenant worktree doctor PATH`.

## Ownership and cleanup

Remove a clean merged worktree with the helper, not raw Git. Before discarding
an unmerged branch or detached commit, obtain authority and record its tip.
The chair decides which other worktrees may be removed. For collection and
retention, see [GC](runbooks/gc.md); for uncertain custody, use
[lane recovery](runbooks/lane-lifecycle.md#recovery). Never delete a lock or
reservation to bypass custody.

The [supervisor spec](specs/supervisor.md#worktrees) owns worktree custody and
shared Git metadata.
