# Shared worktree policy

The [constitution](../HARNESS.md) pre-authorises implementation branches and
linked worktrees. Work in a registered linked worktree under the repository's
`.worktrees/`; keep the primary checkout on the integration branch.

## Primary checkout stays on main

Do not switch branches in the primary checkout, and keep it clean. The chair
merges through the [repository workflow](runbooks/github-workflow.md).

## Canonical location

`<primary-repository-root>/.worktrees/<branch-with-slashes-as-dashes>` is the
shared location across agent platforms. Do not create nested worktrees.

## Helper

```sh
provenant worktree create fix/repo-example --base origin/main
provenant worktree list
provenant worktree remove fix-repo-example
provenant worktree remove fix-repo-example --discard
```

`git worktree add` registers a tree with Git, so `provenant worktree list` can
see it, but writer dispatch also needs Provenant's custody registration.
An advisory Fabric claim does not supply that registration. Fabric refuses
such submissions with `worktree_unregistered` and the worktree path. Create
a writer at the branch's tip with
`provenant worktree create <new-branch> --base <branch>`.

`remove NAME` deletes the clean worktree and then its branch when every commit
on the branch is contained in the fetched upstream default branch. Without
`origin`, or if fetching fails, the primary checkout's integration branch
remains the fallback. An explicit `--integration` always wins. This includes
a branch still at the integration tip, which nothing is lost from. An
unmerged branch is kept and reported as `not_merged`; `--discard` deletes it
too and returns its head, which recovers it with `git branch NAME HEAD` while
Git still holds the objects.

Confinement validates repository identity and refuses primary, dirty
and live writers. `worktree list` reports a tree whose only changes are
ignored files as `ignored`; removal refuses it like a dirty tree, and a
session hand-off treats it as clean. An unknown writer is accepted when its
lease is free and the tree is clean. It reports `git_operation_in_progress`
and refuses removal when the worktree's private Git directory contains an
unfinished cherry-pick, revert, rebase, merge or sequencer operation, even if
the index and working tree are clean. Run calls
`worktrees::check_writer_terminal` before
committing a writer's terminal result, including a result recovered by
reconcile. Unfinished Git operations produce a failed result with the worktree
path and reason. A writer that succeeded but left edits uncommitted with no
new commit ends `uncommitted`, and its reason names the dirty paths.
No force removal is offered.

## Ownership and cleanup

One writer owns each worktree. Removing an authorised merged worktree uses
pinned Git evidence and an explicit branch or merge range:

```sh
provenant worktree remove --merged --integration OID --branch fix/repo-example
provenant worktree remove --merged --integration OID --since PRE_MERGE_OID
```

The integration OID and scope prevent a hook from sweeping unrelated branches.
Install owns fixture-backed hook registration; merged pruning is an explicit
worktree operation. Writer temporary files live in the attempt's
`tmp/candidate-N`; terminal commit attempts removal, and reconciliation
retries once no reservation remains. Guardian settlement removes empty
`scratch/`; unknown scratch contents and retained history stay on disk.
GC retries `tmp/` cleanup for indexed terminal attempts after 24 hours,
including child run stores. It checks reservations and writer locks and
reports paths and allocated bytes; `--dry-run` previews the removals.

Claude Code worktrees follow the same rules: with the managed hooks installed,
Claude creates a native agent's worktree with `provenant worktree create
claude/NAME` and removes it with `provenant worktree remove`, so it has an owner
from the start. See the [Claude hooks runbook](runbooks/claude-hooks.md).

Fabric sets `CARGO_TARGET_DIR`, `CARGO_BUILD_BUILD_DIR` and `CARGO_HOME` for
each lane. Do not override them: confinement may deny other paths. Confined writers use
`$HOME/Library/Caches/provenant-lane-cargo/<sha256>`, derived from canonical
worktree and common Git paths and held directory identities. Same-named and
recreated worktrees get separate caches. Setup requires the cache and lane-only
parent to belong to the effective user with mode 0700, and refuses mismatches.
Native unconfined agents should use one shared per-host target outside
worktrees and this lane namespace. Take care to run the intended checkout's
test binaries when sharing a target. See the
[release build example](runbooks/install.md#build-a-staged-release).

Confined writers also get `PROVENANT_LANE_CARGO_LINT_TARGET`, a `target-lint/`
directory in that cache. For lint-only runs (`cargo check`, `cargo clippy`), set both
`CARGO_TARGET_DIR` and `CARGO_BUILD_BUILD_DIR` to it. Cargo locks the build
directory, so setting only the target directory still queues behind test and
build runs. Worktree removal and `provenant gc` remove this directory with the
cache. Read-only lanes have no lane cache and get no variable.

On macOS, a host may opt writer lanes into cloning the host Cargo registry
into a fresh private Cargo home by setting this in its state `config.toml`:

```toml
[domains.fabric.lanes.cargo_seed]
enabled = true
```

The default is false. Only `~/.cargo/registry` is seeded; Cargo Git stores and
build targets are unchanged. Existing lane registries are retained. Cloning
requires filesystem support for copy-on-write clones and falls back to an
empty registry when unavailable or a source entry cannot be cloned. Other
platforms skip this setting. Each attempt's `cargo-setup.json` records a
`status` (`seeded`, `skipped`, or `fallback`) and a `reason`. Read-only lanes
retain their existing read-only registry access.

`provenant worktree remove` deletes only the removed worktree's own lane cache,
derived from its verified identity before Git removes it. Cache cleanup holds
the writer lock and uses verified detachment before deletion. A cleanup failure
emits a warning; the worktree stays removed. Other orphaned caches and
interrupted cleanup tombstones remain for `provenant gc`.
GC collects idle caches (default seven days), then collects eligible caches
oldest-first by last write to reach the free-space floor. The default floor
is 10% of the state volume's capacity, at least 50 GiB; `gc.free_floor` or
`--free-floor SIZE` overrides it. Both paths protect live writers and
reservations. Writer dispatch refuses to launch below the floor on the
worktree's volume and names `provenant gc` in its error; read-only lanes
still start. GC alerts if the state volume remains below the floor after
collection. macOS default-state installs run GC at load and daily; `[gc]` can
disable or adjust it. Bare `provenant gc` deletes eligible lane caches and
previews merged worktree and declared output cleanup. `--apply` deletes those
entries; `--dry-run` previews every class. The installed agent passes `--apply`.
GC fetches `origin`'s default branch and selects clean managed `.worktrees/`
entries with known-terminal lanes and merged commits of their own. Unmanaged
worktrees are skipped, including for output pruning. Tips
equal to the fetched upstream tip or on its first-parent history are kept,
protecting fresh worktrees. Eligible removal deletes local branches and own
lane caches, keeping dirty, unmerged and live or unobservable worktrees.
Broken registrations are skipped individually. Configured regenerable
output requires a clean or ignored-only managed worktree whose associated
lanes are all known terminal. Ignored output may be pruned; tracked paths,
configured keep paths, symlinks and output written within 24 hours are kept.
See the [configuration runbook](runbooks/configuration.md#lane-cache-collection).
Settlement collects the lane's declared ignored output and Cargo cache without
an idle delay, even when the worktree stays for delivery pickup. Successful
settlement, delivery integration and `land` prune eligible worktrees and
caches. When landing from the merged linked worktree, `land` uses the main
checkout for cleanup and output so it can remove the selected worktree and its
branch. It ignores its own caller ancestors through the session leader in the
worker census, while other observable live processes inside the lane retain it.
An incomplete census or unproved ancestor hop refuses landing with a retry
message; ancestor identities require strictly older native start stamps. The
land process changes cwd before removal and prints a notice when the invoking
shell's directory was removed. The shell must change its own cwd afterwards.
The landing report gives a reason for any cleanup step that is skipped or
refused. Event collection uses the same GC lock as the daily sweep and keeps
foreign worktrees out of scope.
Output pruning holds the worktree's writer lock; the repository lock is held
only for individual worktree removals. Tracked paths and their ancestors are
indexed once per worktree.
Cleanup recognises v1 owner markers for missing owners; existing v1 owners
are kept and never collected as idle.

Never use raw `git worktree remove` on Provenant worktrees; use
`provenant worktree remove`, which holds writer exclusion and cleans the
lane cache.

## Writer dispatch

Agents use `fabric_run` (`op: submit`); this is the terminal path for a shell
without MCP.

```sh
provenant run --operation-id OP --model ADAPTER/MODEL \
  --prompt-file BRIEF --branch BRANCH --base main
```

Fabric uses `access: "worktree_write"` and `worktree`. Both entry points accept
an absolute path or one relative to the selected repository's primary root.
The worktree must have an active Provenant registration for that project.
The primary checkout and another project's worktree are refused. Run reserves
the worktree before compiling its writer permissions, so another writer cannot
launch beside it. The guardian retains the lease until terminal settlement.
At settlement the host best-effort refreshes the writer's branch tracking ref
on `origin`, including for explicit and adopted worktrees, before pinning the
upstream used for pruning. A missing remote or branch is silently skipped;
other failures may be recorded as `tracking_ref_warning` in a lane-owned
settlement receipt and do not change the lane outcome or an adopted receipt.

## Writer custody recovery

A guardian crash leaves `reservation.json` in the worktree registry, even when
every process descriptor closes. A free kernel lock is not proof of provider
exit. Run `provenant reconcile --id RUN --json`: `writer_reservations` names
the exact reservation path and scoped run, attempt and generation. Reconcile
also reports reservations left behind after a terminal commit. Keep the worktree
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
   before settlement; retain unknown files. `Recovery::verify_settlement`
   binds the resulting settlement commit to the original attempt directory
   and scope.
3. With proved absence, call `Recovery::settle`. For an expressly authorised
   acceptance of lost custody, first record `Recovery::reconcile` with the
   chair actor, scope, evidence digest and `AcceptLostCustody` disposition, then
   call `settle_reconciled`. Both paths hold the writer lock while settling
   classified scratch and clearing the reservation.

`land` clears a retained reservation for its merged worktree when supervisor
proves it was never handed off, or its canonical attempt is terminal and
supervisor proves absence and verifies settlement. It checks the worktree/Git
binding and keeps malformed, live, nonterminal or unproved reservations.
Dirty and ignored-output prechecks run under recovered writer exclusion before
any lease clearance; a refused precheck leaves the reservation intact.
Ordinary `worktree remove` still refuses any reservation.
When custody is lost without a durable exit, this revision cannot create the
required terminal receipt, so the fence cannot yet be cleared. Retain it and
use a fresh registered worktree. A failed provider or an unfinished Git
operation with normally settled custody needs Git repair by the chair,
rather than fence clearance.
