# Shared worktree policy
Owner: confinement worktree lifecycle and supervisor custody
Anchor: [`lifecycle.rs`](../tools/front-door/crates/confinement/src/worktrees/lifecycle.rs), [`custody.rs#Recovery`](../tools/front-door/crates/supervisor/src/custody.rs)

The [constitution](../GLOBAL-AGENTS.md) pre-authorises implementation branches and
linked worktrees. Each worktree has one writer. Worktrees isolate changes;
they do not expand authority. The [repository workflow](runbooks/github-workflow.md)
owns integration and landing.

## Primary checkout stays on main

Keep the primary checkout clean on the integration branch; do not switch its
branch to do implementation work.

## Canonical location

Use `<primary-repository-root>/.worktrees/<branch-with-slashes-as-dashes>` across
harnesses. Do not nest worktrees. Git's worktree inventory is the registry;
checkouts created through ordinary Git are immediately usable without a separate
adoption or identity-registration step.

## Helper

```sh
provenant worktree create fix/repo-example --base origin/main
provenant worktree list
provenant worktree remove fix-repo-example
```

`provenant help --all` owns flags and result
fields. New branch names must avoid case, Unicode-normalisation and branch-path
prefix collisions. Remote bases are pinned to the advertised commit, not a
possibly stale tracking ref. Unreachable or ambiguous remote bases refuse
creation. The implementation owner is
[`inventory.rs`](../tools/front-door/crates/confinement/src/worktrees/inventory.rs).

New worktrees can run [project setup commands](runbooks/configuration.md#worktree-setup)
before a writer starts. Setup failures warn and preserve the checkout; reusing
a checkout does not rerun setup.

Removal preserves dirty trees, active writers and unfinished Git operations.
Ignored-only trees are dirty for removal even though a session handoff may treat
them as clean. A held writer lock proves exclusion, not a live process; observed
processes provide separate evidence. Landing requires a complete census, while
ordinary removal can proceed with a free lock despite an incomplete census.

A clean merged branch can be removed with its worktree. An unmerged branch or
detached commit needs explicit discard authority; preserve its reported head
before discarding. Never use raw `git worktree remove` for a Provenant worktree:
the Provenant operation holds writer exclusion and removes its own lane cache.

## Ownership and cleanup

Merged pruning is an explicit, scoped worktree operation using a pinned
integration commit and branch or merge range. It must not sweep unrelated
branches. The chair decides which other worktrees may be removed and preserves
unmerged tips. Managed Claude worktree hooks use the same lifecycle owner.

Collection preserves live or unobservable custody, tracked files, configured
keep paths and unknown recovery evidence. Standalone output collection has an
age threshold; settlement and removal of a proven merged tree can collect its
declared regenerable output immediately. The [GC runbook](runbooks/gc.md) owns
collection operations and eligibility settings. Stable lock files remain outside disposable trees:
unlinking and recreating a lock name can split exclusion across inodes.

Confined writers use private Git objects, refs and merge state. The shared branch
stays at its launch base until host settlement verifies and imports objects and
updates the expected ref. Scripts must not mistake this private Git layout for
an unregistered worktree. Use `--show-toplevel` for the checkout and
`PROVENANT_LANE_DEPTH` for lane identity. The [run spec](specs/run.md#lane-git-and-import)
owns the import contract; [lane lifecycle](runbooks/lane-lifecycle.md#recovery)
owns recovery and upstream integration.

Fabric supplies the lane Cargo home and build directories. Keep those bindings;
confinement may deny other paths. Cargo seed and cache mechanisms belong to the
[run spec](specs/run.md#cargo-target-seed), retention settings to
[GC](runbooks/gc.md#what-is-removed-and-when).
[`measure-cargo-target-seed`](../scripts/measure-cargo-target-seed) is a host-only measurement tool; experimental timing
results belong in their issue, not this policy.

## Repository mutation ownership

Worktree registry operations and writer Git setup share
`provenant-repository.lock` in the Git common directory. Shared holders read
configuration snapshots; exclusive holders add or remove worktrees or write
configuration and retain the lock across the snapshots those writes depend on.
When writer custody is already held, blocking acquisition follows `writer.lock`
then the repository lock. A repository holder may only try a writer lock without
waiting, so the opposite direction cannot create a blocking cycle. Removal
prepares regenerable output and refreshes its census under writer custody before
taking the repository lock for final rechecks, Git removal and ref settlement.
Slow census,
network discovery and provider execution run without the repository lock; a
final custody recheck may run under the mutation owner. The lock inode stays in
place for the repository's lifetime.

## Writer dispatch

Use the [lane lifecycle runbook](runbooks/lane-lifecycle.md) to explain and submit
a writer. An existing writer worktree must be in the selected project's Git
inventory and cannot be the primary checkout or another project's tree.
Admission reserves it before compiling permissions; custody retains exclusion
until terminal settlement. The reservation and lock are evidence for recovery,
not another worktree-membership registry.

## Writer custody recovery

The kernel `writer.lock` is the exclusion authority. Its descriptor is inherited
by descendants, so a guardian crash does not free the writer while a descendant
still holds it. Never delete a lock or reservation to force admission.

Inspect `provenant reconcile --id RUN --json` for the exact scoped attempt and
reservation. A free lock alone does not prove that the result was imported.
Reconcile can recover a durable exit or prove loss after a reboot using recorded
boot and directory bindings; an unchanged boot is not absence evidence.

Host recovery uses [`custody.rs#Recovery`](../tools/front-door/crates/supervisor/src/custody.rs)
under authority for that reservation, holding writer exclusion throughout.
Automatic withdrawal is limited to proved unlaunched preparation. A handed-off
writer needs terminal publication and verified settlement, or an expressly
authorised acceptance of lost custody with its evidence recorded. Unknown files
and uncertain custody stay fenced. A completed supervisor settlement resolves
its exact attempt and generation even if reservation removal was interrupted;
a historical import marker alone does not fence the worktree. An unresolved
import refusal names its marker, run and attempt, with commands to reconcile
or explicitly abandon it while preserving private Git evidence. Stopping a
terminal lane does not abandon an unresolved import. Ordinary Git failure after
settled custody needs Git repair, not fence clearance. Use a fresh linked worktree while the old
writer remains busy; retain its evidence for recovery.
