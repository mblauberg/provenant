# Shared worktree policy

The [constitution](../HARNESS.md) pre-authorises implementation branches and
linked worktrees. Work in a registered linked worktree under the repository's
`.worktrees/`; keep the primary checkout on the integration branch. The primary
Provenant checkout is the frozen live install until authorised cut-over.

## Primary checkout stays on main

Never switch, pull or reinstall the live primary checkout during the rewrite.
The chair merges through the [repository workflow](runbooks/github-workflow.md).

## Canonical location

`<primary-repository-root>/.worktrees/<branch-with-slashes-as-dashes>` is the
shared location across agent platforms. Do not create nested worktrees.

## Helper

```sh
provenant worktree create fix/repo-example --base origin/main
provenant worktree list
provenant worktree remove fix-repo-example
```

Rust confinement validates repository identity and refuses primary, dirty,
live or unknown writers. No force removal is offered.

## Ownership and cleanup

One writer owns each worktree. Removing an authorised merged worktree uses
pinned Git evidence and an explicit branch or merge range:

```sh
provenant worktree remove --merged --integration OID --branch fix/repo-example
provenant worktree remove --merged --integration OID --since PRE_MERGE_OID
```

The integration OID and scope prevent a hook from sweeping unrelated branches.
The old post-merge and pre-push shell adapters are retired. Rust install owns
fixture-backed hook registration; merged pruning is an explicit Rust worktree
operation. The clean/reap engines, retention plans and Node dependency cloning
are retired. Guardian settlement owns classified scratch cleanup; retained
history stays on disk.
