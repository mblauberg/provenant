# Shared worktree policy

The project constitution (`HARNESS.md`) is a standing user-approved envelope:
creating linked worktrees for implementation work needs no per-instance
approval. That authority still does not imply authority to delete a branch,
force-remove state or let agents write overlapping scopes — each of those
remains separately gated. Merge authority is repo-based; see the repository's
workflow runbook (for this repo, `docs/runbooks/github-workflow.md`).

## Canonical location

Every authorised linked worktree is a direct child of the owning repository's
primary checkout:

```text
<primary-repository-root>/.worktrees/<task-agent>
```

All agent platforms use that same directory. Never place linked worktrees in a
platform cache, `/tmp`, a home-level pool, the current linked worktree, or an
artifact `scaffolds/` directory. A nested repository or submodule owns its own
`.worktrees`. Multi-repository work uses one authorised worktree per repository.

Project instructions may strengthen this invariant. Only a direct user
instruction may make a one-run location exception.

## Helper

Use the checked helper after authorisation. `--human-authorised` attests that
the current operation is covered by a direct instruction or an active approved
envelope; the run receipt records which source supplied that authority:

```sh
scripts/worktree create NAME --human-authorised --detach REV
scripts/worktree create NAME --human-authorised --new-branch BRANCH \
  --branch-authorised --start-point REV
scripts/worktree create NAME --human-authorised --existing-branch BRANCH
scripts/worktree list
scripts/worktree check
scripts/worktree remove NAME --human-authorised
```

The helper resolves the primary checkout through Git's common directory, checks
the name and protected root, and refuses unsafe creation/removal. Receipts for a
run record the selected `repo_root`, `primary_root`, `common_git_dir`, worktree
path and branch/detached state.

## Ownership and cleanup

- One stage owner writes a worktree at a time. Sibling agents use separate
  worktrees or artifact-only scopes.
- Worktrees share Git objects, configuration and hooks; they are not security
  sandboxes. Secrets, LFS and submodules need their own deliberate setup.
- Worktrees do not share ignored dependencies or build output. Before treating
  a suite failure as a product defect, run the owning repository's declared
  lockfile bootstrap and build commands in that worktree, then record the exact
  commands and results. Do not guess the package manager or install mode.
- Before removal, confirm a clean status, no live agent/pane and no unconsumed
  handoff. Use `git worktree remove`, never filesystem deletion.
- Force removal of a dirty worktree, and deletion of an unmerged branch, require
  separate user authority. Post-merge cleanup does not; see below.
- `.worktrees/` is protected infrastructure: context cleaners, broad backups
  and scratch pruning must skip it.

## Post-merge pruning

A merge is the authority-bearing event for cleaning up after itself. Once a
branch is merged into the integration branch, the agent that observed the merge
prunes that branch's own artefacts without a further user gate. This is standing
across every project the harness is loaded into: a merged worktree left behind
is stale state that later agents mistake for live work, and each one carries its
own uninherited dependency tree — in this repository roughly 400 MB apiece.

Prune immediately after the merge, in this order:

```sh
git -C <primary-root> fetch origin
git -C <primary-root> merge --ff-only origin/<integration-branch>
git -C <primary-root> branch --merged <integration-branch>   # confirm before deleting
scripts/worktree remove <name> --human-authorised
git -C <primary-root> branch -d <merged-branch>
git -C <primary-root> worktree prune
git -C <primary-root> remote prune origin
```

Constraints that keep this narrow:

- **Confirm the merge first.** `git branch --merged` must list the branch, or
  `gh pr view <n> --json state` must report `MERGED`. Never infer a merge from a
  green pull request or a passing suite.
- **Only that branch's artefacts.** The authority covers the worktree created
  for the merged branch, that branch's local ref, and stale remote-tracking refs
  for branches the forge already deleted. It does not extend to any other
  worktree, branch or checkout.
- **`-d`, never `-D`.** If Git refuses the delete, the branch is not fully
  merged: stop and report rather than forcing. `git worktree remove` likewise
  stays unforced — a dirty worktree is unconsumed work, not debris.
- **Retention beats reclamation.** Where a repository requires a run directory,
  receipt or artifact to survive the merge, that requirement wins; prune only
  after it is satisfied. Check the repository's own workflow runbook.
- **The primary checkout stays on the integration branch.** Pruning never leaves
  it on a deleted or detached ref.

Pruning is not conditional on disk pressure. Do it as the last step of the merge,
not as a later sweep.
