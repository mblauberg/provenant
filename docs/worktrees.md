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

Use the checked helper after authorisation. `--human-authorised` and
`--branch-authorised` are caller attestations; the helper does not record
whether authority came from a direct instruction or an active approved
envelope:

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
the name and protected root, and refuses unsafe creation/removal. A create
receipt emits `primary_root`, `worktree_root`, `common_git_dir`, `head_revision`
and branch/detached state. It does not emit the supplied repo path as
`repo_root`; record that path and the authority provenance separately when the
run contract requires them. A removal receipt emits only `status`, `name` and
`primary_root`.

## Ownership and cleanup

- One stage owner writes a worktree at a time. Sibling agents use separate
  worktrees or artifact-only scopes.
- Worktrees share Git objects, configuration and hooks; they are not security
  sandboxes. Secrets, LFS and submodules need their own deliberate setup.
- Worktrees do not share ignored dependencies or build output. Before treating
  a suite failure as a product defect, run the owning repository's declared
  lockfile bootstrap and build commands in that worktree, then record the exact
  commands and results. Do not guess the package manager or install mode.
- Live tooling can point into the primary checkout, so moving its branch breaks
  things a clean Git status will not reveal. MCP server registrations and
  `~/.local/bin` symlinks both name paths inside the checkout, and they follow
  the working tree rather than a branch. Before moving the primary checkout,
  check for consumers that resolve into it; if any exist, do the work in a
  linked worktree, or repoint the consumer first and then invoke the installed
  command to confirm it still resolves. Editing a template in the repository
  does not update an already installed copy: rerun the installer and exercise
  the installed command, because a clean tree is not evidence that anything
  outside the tree still works.
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
# 1. Establish where you are. Both must match before anything mutates.
test "$(git -C <primary-root> rev-parse --show-toplevel)" = "<primary-root>"
test "$(git -C <primary-root> symbolic-ref --quiet --short HEAD)" = "<integration-branch>"

# 2. Sync the integration branch.
git -C <primary-root> fetch origin
git -C <primary-root> merge --ff-only origin/<integration-branch>
```

Then choose the proof that matches how the pull request merged. For a merge
commit, ancestry is the gate:

```sh
git -C <primary-root> merge-base --is-ancestor <merged-branch> <integration-branch>
```

For a squash merge, ancestry cannot hold. The pull request must report
`MERGED` with the expected refs and the scoped content diff must be empty:

```sh
gh pr view <n> --json state,headRefName,baseRefName
git -C <primary-root> diff <integration-branch> <merged-branch> -- <paths the branch touched>
```

Only after the applicable proof passes, prune that branch's artefacts. Use
`-d` after a merge commit, or `-D` only after the squash content proof:

```sh
scripts/worktree remove <name> --repo <primary-root> --human-authorised
git -C <primary-root> branch -d <merged-branch>  # merge commit
# git -C <primary-root> branch -D <merged-branch>  # squash merge only
git -C <primary-root> worktree prune
git -C <primary-root> remote prune origin
```

Constraints that keep this narrow:

- **Establish the repository and branch before mutating.** `merge --ff-only`
  fast-forwards whatever `<primary-root>` currently has checked out, and the
  helper acts on its own current repository unless given `--repo`. A prune run
  from the wrong checkout, or against a detached HEAD, silently moves the wrong
  ref. Step 1 is not optional.
- **Prove the merge, not the status — and use the proof that fits the merge.**
  For a merge commit,
  `git merge-base --is-ancestor <merged-branch> <integration-branch>` exiting 0
  is the proof, because it names both refs explicitly and is cheap.

  A **squash merge produces a new commit with no ancestry link**, so that gate
  refuses a branch that is fully merged. An agent following it either stalls or
  reaches for the force-delete the policy exists to prevent. When the pull
  request was squash-merged, prove *content* instead: the pull request
  reports `MERGED`, and

  ```sh
  git -C <primary-root> diff <integration-branch> <merged-branch> -- <paths the branch touched>
  ```

  is empty. **Scope it to the paths the branch touched.** An unscoped diff also
  reports everything the integration branch gained *after* the branch was cut,
  which reads as divergence: one prune showed 124 deletions that all came from an
  unrelated later merge, and was empty once scoped to its two files.

  `gh pr view <n> --json state` reporting `MERGED` is not sufficient alone: it
  says nothing about which repository the pull request belongs to, nor whether
  its head and base refs are the branches in front of you. Verify `headRefName`
  and `baseRefName` too. Never infer a merge from a green pull request or a
  passing suite.
- **Only that branch's artefacts.** The authority covers the worktree created
  for the merged branch, that branch's local ref, and stale remote-tracking refs
  for branches the forge already deleted. It does not extend to any other
  worktree, branch or checkout.
- **`git branch -d` is not a merge check.** Git deletes a branch that is merged
  into *its upstream*, even when it is not merged into HEAD, warning and exiting
  0. Every pushed implementation branch has an upstream, so this is the common
  case, not the exotic one. Verified directly:

  ```text
  warning: deleting branch 'feat' that has been merged to
           'refs/remotes/origin/feat', but not yet merged to HEAD
  Deleted branch feat (was 95ef3c3).
  ```

  It fails the other way too: after a **squash** merge `-d` refuses a branch that
  is fully merged, because it looks for ancestry that no longer exists. Both
  directions mislead, which is why step 3 is the safeguard and `-d` is only a
  second line of defence.

  After a merge commit, use `-d`; if Git refuses despite step 3 passing,
  something disagrees with your model of the repository, so stop and report.
  After a squash merge whose content gate in step 3 came back empty, `-D` is the
  correct command and is covered by this standing authority — the proof has
  already been made, and `-d` cannot see it. `-D` on a branch whose step-3 gate
  did *not* pass is still a force-delete needing separate authority.
  `git worktree remove` stays unforced either way — a dirty worktree is
  unconsumed work, not debris.
- **Retention beats reclamation.** Where a repository requires a run directory,
  receipt or artifact to survive the merge, that requirement wins; prune only
  after it is satisfied. Check the repository's own workflow runbook.
- **The primary checkout stays on the integration branch.** Pruning never leaves
  it on a deleted or detached ref.

Pruning is not conditional on disk pressure. Do it as the last step of the merge,
not as a later sweep.
