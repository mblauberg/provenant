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

New branch names must not differ from an existing local or remote-tracking
branch name only by case (including non-ASCII case) or by a branch-path
prefix, and spellings that differ only in Unicode normalisation (NFC against
NFD) count as the same name. Branch names follow Git's own ref
format, so `feat/a+b` is accepted.

A remote base such as `origin/main` is pinned to the commit the remote
advertises, rather than the tracking ref, and revision operators apply to that
commit (`origin/main~1`). This holds for every spelling of the tracking root
(`remotes/origin/HEAD~1`, `refs/remotes/origin/main^`, `origin`, `@{u}`).
Remote bases require a reachable remote; an offline or missing remote base
fails instead of trusting a stale tracking ref, and so does a base that Git
resolves to a remote-tracking ref the remote did not advertise (such as
`Origin/main` on a case-insensitive filesystem).

Host origin refresh leaves out advertised branches whose names collide with
another advertised name, with a warning; their tracking refs may remain
stale. `HEAD` counts as an existing name, so a branch called `head` collides
with `origin/HEAD`, and a branch that would be written into an existing
tracking symref is left out too. It fails only if the remote's default branch
is among them. Refresh fetches pass `core.precomposeunicode=true`, so names
that differ only in Unicode normalisation (an NFD default with an NFC sibling)
would share one tracking ref. Refresh compares every advertised name, and every
tracking ref already on disk, under NFC normalisation and case folding before
it fetches, and refuses outright, naming both spellings, when two distinct
spellings are equivalent. The
pre-push policy reads the remote's advertised default branch instead of the
local tracking HEAD, and refuses the push if none is advertised.

`remove NAME` deletes the clean worktree and then its branch when every commit
on the branch is contained in the fetched upstream default branch. Without
`origin`, or if fetching fails, the primary checkout's integration branch
remains the fallback. An explicit `--integration` always wins. This includes
a branch still at the integration tip, which nothing is lost from. An
unmerged branch is kept and reported as `not_merged`; `--discard` deletes it
too and returns its head, which recovers it with `git branch NAME HEAD` while
Git still holds the objects.

A registered checkout with a detached HEAD can be removed by its directory
name. The same custody and dirty checks apply, and no branch is deleted.
If its HEAD is not contained in the integration branch, removal names the
commit and refuses unless `--discard` is explicit. Preserve that commit with
`git -C PATH branch NAME HEAD` before retrying with `--discard`, or use `--discard`
when the detached work is no longer needed. A detached primary checkout needs
an explicit `--integration BRANCH`.

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

A custody refusal names its reason (a stale registration names the recorded
field that differs, as a tree re-added with `git worktree add` at a registered
path does; a locked or prunable Git entry; a reservation; an unreadable record)
and a repair. `worktree adopt PATH --owner NAME`, run from outside the tree,
replaces a stale registration. It still refuses a held writer lock, a
reservation and a live process, and names the process by pid and command, as
`pr land` does.
An incomplete native process census alone does not identify a live writer:
ordinary adoption, like removal, can proceed while holding the exclusive writer
lock with no reservation. A positive live-process observation still refuses.

When a filesystem renumbers its device, a registered tree can refresh that
number under the worktree registry lock. Its recorded root inode, path, branch
and project must still match, and Git must confirm the linked worktree and its
common directory and backlink. New registrations and refreshed legacy records
also retain the common and private Git directory identities for later checks.
The registration records the number of device refreshes. Other identity
changes remain stale and require adoption.

`worktree list` prints one terse line per tree. The path is a plain path,
quoted only when it needs it; `--json` keeps the structured path object.

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

Confined writer lanes use private Git objects, refs and merge state. Their shell
Git wrapper pins the linked worktree and index. Shared Git metadata, including
objects, branch refs, remote-tracking refs, reflogs and config, stays read-only
apart from the lane's index and index lock. The shared lane branch stays at its
launch base while the lane runs. At settlement, the host verifies object contents
before importing them without replacing existing objects, verifies the shared
graph, then updates the selected branch against its launch base before reading
lane history or delivering it. Before launch, the host refreshes origin and
prepares the lane's private ref snapshot and upstream. A lane can commit on its
branch and run `git push origin HEAD:<branch>`.
To pick up upstream changes, run `git fetch --refmap= origin main` and use
`FETCH_HEAD` through the same wrapper. Lane-local tracking refs are untrusted
copies; host integration refreshes and verifies its own refs and objects.
Host delivery preparation constrains fetches to the selected remote's tracking
refs and redacts URL userinfo from preparation errors.

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
and Git store into a fresh private Cargo home by hand-editing its state `config.toml` (this
setting is not supported by `provenant config set`):

```toml
[domains.fabric.lanes.cargo_seed]
enabled = true
max_generations = 3
max_bytes = 21474836480
```

The default is false. `~/.cargo/registry` and `~/.cargo/git` are seeded independently;
existing lane stores are retained. Cloning
requires filesystem support for copy-on-write clones and falls back to an
empty registry when unavailable or a source entry cannot be cloned. A symlink
or other unsupported entry anywhere in a host store causes that whole store
to fall back on every fresh lane until the entry is removed or the store is
recreated. Inspect the host store and the `registry_contains_unsupported_entry`
or `git_contains_unsupported_entry` reason in the attempt record to diagnose this.
Other platforms skip this setting. Each attempt's `cargo-setup.json` (registry)
and `cargo-git-setup.json` (Git) record a `status` (`seeded`,
`skipped`, or `fallback`) and a `reason`. If the record cannot be written, the
host emits a warning and the lane still runs. A `durability_unconfirmed` reason
means the store was published but its directory sync failed. Read-only lanes
retain their existing read-only registry access.

To prepare a target seed, run `provenant worktree seed-build` on the host from
the project. It builds `tools/front-door` from the pinned local `origin/main`
commit in a scratch checkout, using `cargo build --locked --all-targets` and
`cargo test --locked --no-run`. A successful build is published as a generation
under the state directory's `cargo-target-seeds/generations/`. The manifest
records the commit, full `rustc -vV`, host triple, profile, features and flags.
Refresh `origin/main` before building when a newer seed is wanted. This is a
host operation: only build seeds from a trusted `origin/main`.

Fresh writer targets are cloned from the newest generation whose full rustc
version and host triple match the lane's toolchain. Cargo fingerprints decide
which units can be reused when source, profile, features or flags differ. The
host probes an installed release compiler directly, without executing a toolchain
or binary from the writer checkout. Verification requires an explicit fixed
release pin in `tools/front-door/rust-toolchain.toml` or the legacy
`rust-toolchain`, an unambiguous host installation, and no rustup overrides or
lane environment overrides. The lane's PATH must resolve to the protected host
rustup shim, the lane must not be able to write its toolchain, and Cargo config
files that could select another compiler cause a fallback. If verification
fails, the target falls back with `toolchain_unverified`. The lane writes only
its own target; it cannot promote output into the seed. An existing lane target
is retained. `cargo-target-setup.json` records whether
preparation seeded, skipped or fell back, including mismatch reasons. The lane
starts with an empty target if no matching generation can be cloned. The
configuration above limits retained generations to three and 20 GiB of apparent
file size; `provenant gc` removes the oldest generations over either limit.
It also reclaims abandoned build staging trees once no seed build is active,
and removes incomplete generations. Only one host seed build runs at a time.
Generations created before the completion marker was introduced are treated
as incomplete; build a fresh generation after upgrading.
GC skips the store while a lane is cloning. Zero for either limit removes all
generations at the next GC.

To compare on each host, build a generation, then run the following from the
same checkout on an unconfined host. Substitute the generation directory shown
by `seed-build` and a new output directory on the same APFS volume:

```sh
scripts/measure-cargo-target-seed \
  "$HOME/.local/share/provenant/v1/cargo-target-seeds/generations/GENERATION" \
  "$HOME/seed-measurement" > seed-measurement.txt
```

The script clones each file with `clonefile`, failing if cloning is unavailable,
then times `cargo check --locked` and `cargo test --no-run --locked` in separate
cold and seeded targets. It reports each target's apparent and allocated bytes
using `du`, plus the volume free-space change during cloning. `du` counts shared
blocks for each clone, so it cannot determine shared bytes; the free-space
change is only an estimate and can include concurrent volume activity. Record
the manifest, checkout and toolchain printed with the timings. The output targets
are retained for inspection. These measurements are still pending on both hosts;
there is no evidence yet to enable seeding by default.

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
branch. The primary checkout comes from Git's worktree inventory. When Git
lists a separate administrative directory instead, `land` checks `core.worktree`
or a containing checkout. An out-of-tree lane needs `core.worktree` set if
the inventory does not name the primary checkout; otherwise landing reports
the inventory problem. It ignores its own caller ancestors through the session
leader in the worker census, while other observable live processes inside the
lane retain it.
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

After a reboot, reconcile can settle an attempt as lost and release its
reservation when the recorded boot and directory bindings prove the previous
process tree is gone. If device numbers changed, recovery requires the recorded
attempt and reservation to have shared a filesystem and their current directories
to share one too, with both recorded inodes still matching. If this evidence is
missing, retain custody and inspect the reservation and boot identity before
chair recovery. An unchanged boot does not establish absence.

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
When custody is lost without a durable exit or reboot proof, the fence cannot
yet be cleared. Retain it and use a fresh registered worktree. A failed provider or an unfinished Git
operation with normally settled custody needs Git repair by the chair,
rather than fence clearance.
