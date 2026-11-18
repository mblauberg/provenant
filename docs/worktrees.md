# Worktrees
Owner: confinement worktree lifecycle and supervisor custody
Anchor: [`lifecycle.rs`](../tools/front-door/crates/confinement/src/worktrees/lifecycle.rs)

Use linked worktrees for implementation and keep the primary checkout on the
integration branch. Use `<primary-repository-root>/.worktrees/<branch-with-slashes-as-dashes>`;
do not nest worktrees. `provenant run --branch` uses that primary checkout and
its project port configuration even when invoked from a linked checkout or a
subdirectory. An explicit relative `--worktree` path remains relative to the
invoking directory.

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

Remove a clean merged worktree with the helper. The chair promptly removes
landed, superseded or abandoned worktrees and branches under the global Git
rule; record removed tips and preserve live writers and uncommitted edits. For
collection and retention, see [GC](runbooks/gc.md); for uncertain custody, use
[lane recovery](runbooks/lane-lifecycle.md#recovery). Never delete a lock or
reservation to bypass custody.

The [supervisor spec](specs/supervisor.md#worktrees) owns worktree custody and
shared Git metadata.
