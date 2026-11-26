# Worktrees
Owner: confinement worktree lifecycle and supervisor custody
Anchor: [`lifecycle.rs`](../tools/front-door/crates/confinement/src/worktrees/lifecycle.rs)

Use linked worktrees for implementation and keep the primary checkout on the
integration branch. Use `<primary-repository-root>/.worktrees/<branch-with-slashes-as-dashes>`;
do not nest worktrees. `provenant run --branch` uses that primary checkout and
its project port configuration even when invoked from a linked checkout or a
subdirectory. An explicit relative `--worktree` path remains relative to the
invoking directory.

The worktree-layout port may declare roots outside the checkout. A root in the
repository's `.provenant/ports.json` may be absolute or parent-relative (for
example `../writers`), with a matching `default` template such as
`../writers/<branch>`; the built-in remains `.worktrees/<branch>`. A declared
root is treated exactly like the default one for creation, listing, custody and
GC, and `provenant config ports` reports each root with the layer and origin
that declared it. Keep roots private to the owner and on one filesystem: the
worktree parent is created under exclusive custody with the same identity and
permission checks as the default layout.

```sh
provenant worktree create fix/repo-example --base origin/main   # base defaults to the current head
provenant worktree list                                         # path, lane and base per checkout
provenant worktree locate fix/repo-example                      # also lane, attempt or PR number
provenant worktree remove fix-repo-example                      # --discard also deletes an unmerged branch
```

`provenant worktree locate QUERY` answers "where is the checkout for this
branch, lane, attempt or pull request?" for chairs. QUERY may be a full or
short branch, a lane name, an attempt id or a PR number (with or without `#`).
In human mode it prints the path alone, so it composes with the shell:

```sh
cd "$(provenant worktree locate fix/repo-example)"
```

With `--json` it returns `{path, branch, lane, base}` inside the command
envelope. A query that matches nothing is refused with close matches. The same
`path` and `base` are carried on `provenant status --json` lane rows.

`provenant worktree remove` refuses only a dirty checkout or one a live writer
holds. It records `{branch, tip, path, at}` in `<state>/worktrees/removed.jsonl`
(`provenant.worktree-removed.v1`) and prints the record. It deletes the local
branch when it is merged into its base; an unmerged branch is kept and named.

Options are in `provenant worktree --help`; project setup is in
[configuration](runbooks/configuration.md#worktree-setup). If a worktree is
refused, inspect it with `provenant worktree doctor PATH`.

## Ownership and cleanup

Remove a clean merged worktree with the helper. The chair promptly removes
landed, superseded or abandoned worktrees and branches under the global Git
rule; record removed tips and preserve live writers and uncommitted edits. For
collection and retention, see [GC](runbooks/gc.md); for uncertain custody, use
[lane recovery](runbooks/lane-lifecycle.md#recovery). When an attempt record no longer decodes,
`provenant reconcile` or `release attempt:ID` reclaims its dead lease only after supervisor
evidence proves quiescence under writer exclusion, preserving the bytes under `quarantine/`.
Completed exit evidence can prove same-boot quiescence; an exit still settling keeps the fence.
Never delete a lock or
reservation to bypass custody.

Creation takes exclusive custody of its destination before checking the path,
Git registration or branch, and repeats those checks under custody. State left
by a failed `git worktree add` is retained unless its identity can be
attributed to that invocation; an earlier absence check alone does not
authorise recursive cleanup. The [supervisor spec](specs/supervisor.md#worktrees)
owns worktree custody and shared Git metadata.
