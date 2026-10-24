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
live or unknown writers. It reports `git_operation_in_progress` and refuses
removal when the worktree's private Git directory contains an unfinished
cherry-pick, revert, rebase, merge or sequencer operation, even if the index
and working tree are clean. Run calls `worktrees::check_writer_terminal` before
committing a writer's terminal result, including a result recovered by
reconcile. Unfinished Git operations produce a failed result with the worktree
path and reason.
No force removal is offered.

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

Never share `CARGO_TARGET_DIR` across worktrees. Confined writers use
`$HOME/Library/Caches/provenant-lane-cargo/<sha256>`, derived from canonical
worktree and common Git paths and held directory identities. Same-named and
recreated worktrees get separate caches. Setup requires the cache and lane-only
parent to belong to the effective user with mode 0700, and refuses mismatches.
Host builds must use separate target directories outside this lane namespace;
see the [release build example](runbooks/front-door.md#build-a-staged-release).

## Writer dispatch

```sh
provenant dispatch --operation-id OP --model ADAPTER/MODEL \
  --prompt-file BRIEF --mode worktree-write --worktree .worktrees/BRANCH
```

Fabric uses `access: "worktree_write"` and `worktree`. Both entry points accept
an absolute path or one relative to the selected repository's primary root.
The worktree must have an active Provenant registration for that project.
The primary checkout and another project's worktree are refused. Run reserves
the worktree before compiling its writer permissions, so another writer cannot
launch beside it. The guardian retains the lease until terminal settlement.

## Writer custody recovery

A guardian crash leaves `reservation.json` in the worktree registry, even when
every process descriptor closes. A free kernel lock is not proof of provider
exit. Run `provenant reconcile --id RUN --json`: `writer_reservations` names the
exact reservation path and scoped run, attempt and generation. Reconcile also
reports reservations left behind after a terminal commit. Keep the worktree
reserved while custody is uncertain; do not remove the lock or reservation by
hand, or rerun the same writer alongside it.

The chair clears a fence through supervisor's existing recovery API, under
authority for that exact reservation:

1. Acquire `custody::acquire_recovery` on the reported worktree state directory.
   Check its scope against the report. Native process identities and the
   recorded boot/session evidence determine whether custody is absent. A
   reservation still in `Prepared` with no guardian, provider or bound attempt
   can be cleared by `Recovery::withdraw_prepared`; reconcile performs this
   safe prelaunch withdrawal automatically.
2. For a handed-off writer, recover any durable exit through run's ordinary
   terminal path. A committed terminal and quarantine publication are required
   before settlement; retain unknown files. `Recovery::verify_settlement` binds the resulting
   settlement commit to the original attempt directory and scope.
3. With proved absence, call `Recovery::settle`. For an expressly authorised
   acceptance of lost custody, first record `Recovery::reconcile` with the
   chair actor, scope, evidence digest and `AcceptLostCustody` disposition, then
   call `settle_reconciled`. Both paths hold the writer lock while settling
   classified scratch and clearing the reservation.

The CLI does not expose this clearance yet: `--abandon-uncertain` still refuses.
When custody is lost without a durable exit, this revision cannot create the
required terminal receipt, so the fence cannot yet be cleared. Retain it and use
a fresh registered worktree. A failed provider or an unfinished Git operation
with normally settled custody needs Git repair by the chair, rather than fence
clearance.
