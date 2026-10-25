# GitHub work-item workflow

Use this process for user- and agent-originated repository work. Project
Status is the sole workflow state; labels describe content or gates, not
progress. The label taxonomy is declarative in
[`.github/labels.yml`](../../.github/labels.yml), synced with pruning: a label
absent from that file is deleted on the next sync, so never hand-create one.
Issue intake uses the issue forms under `.github/ISSUE_TEMPLATE/`; blank
issues are disabled.

## Statuses

| Status | Meaning |
|---|---|
| Backlog | Untriaged or explicitly deferred. |
| Ready | Accepted with bounded scope, authority and acceptance evidence. |
| In progress | An owner is executing the accepted work. |
| In review | The pull request, checks or independent review is active. |
| Awaiting user | A user decision remains, including explicit acceptance for terminal work or approved scope. |
| Done | The item is integrated, or closed with its terminal reason recorded. |

Delivery acceptance is met by objective checks and tier-required review. The
chair reports the outcome and the user may still reject it. Explicit user
acceptance is required only for `terminal` work or when the approved scope
asks for it. A project may name its landing gate as acceptance; this does not
grant merge or promotion authority.

## Triage

1. Check the evidence, desired outcome, scope, acceptance evidence,
   dependencies, risk, authority and user gates.
2. Record one result in a comment:
   - **Accepted:** state the bounded scope, authority and remaining gates; set
     `Ready`.
   - **Rejected:** give an evidence-based reason and a condition that would
     justify reopening; set `Done` and close as not planned.
   - **Deferred:** give the reason and reconsideration condition; leave open in
     `Backlog`.
   - **Duplicate:** link the canonical item; set `Done` and close as not planned.
3. Agents may triage only inside their granted authority. A spec, one-way-door
   choice, required explicit acceptance or release decision moves to `Awaiting user`; an
   agent never infers it. User-originated items follow the same evidence and
   scope checks.

### Native sub-issues

The parent issue owns change scope, story and remaining gates. Native child
issues own independently deliverable slices. Create or link a child through
GitHub's `sub_issues` API, not a checklist or title.

The POST requires explicit external-write authority. Without it, return the
proposed parent/child relation and do not call `gh`.

When authorised, use the numeric issue id:

```sh
child_id=$(gh api repos/OWNER/REPO/issues/CHILD_NUMBER --jq .id)
gh api --method POST repos/OWNER/REPO/issues/PARENT_NUMBER/sub_issues \
  -F sub_issue_id="$child_id"
```

The parent URL in a child body is navigation only and is not the relationship.
Do not mirror children in a checklist. Closing the parent remains separately
gated after its children and all parent-level acceptance or release gates are
handled.

## Execute and review

1. Set `In progress` when an owner starts the accepted scope.
2. Link a pull request with `Closes #N` only when merge leaves no user or
   external-action gate. Otherwise use `References #N` and keep the issue open.
3. Set `In review` while exact-head checks and independent review run. A new
   commit invalidates that exact-head evidence and it must be rerun.
4. If machine gates pass but a user gate remains, set `Awaiting user`.
5. A user decision comment names the selected choice, artifact or exact head,
   supporting evidence and every remaining gate.
6. Set `Done` only after the issue has no remaining gate and its terminal
   reason or integrated pull request is recorded.

Reach for a Mermaid diagram whenever it makes a change faster to understand than
prose: GitHub renders Mermaid in issues, pull requests and comments. Flows and
roundtrips, before/after refactors, state machines, decision trees for open
decisions, and cross-issue dependencies are strong candidates; a table is the
right visual for tabular data. Prefer one wherever it saves reviewer attention,
not only when prose would be hard. Skip it only when the change is simple enough
that prose is already clear at a glance. Keep every diagram small, legible and
captioned, because a diagram that does not save the reader time is noise, so cut it.

## Mechanics

This runbook is provenant-local repository process, not harness doctrine. The
worktree authority it relies on (the standing envelope in `HARNESS.md` and the
helper contract in [`docs/worktrees.md`](../worktrees.md)) is harness doctrine
and applies wherever the harness is loaded.

The loop below takes an accepted issue from `Ready` to `Done`. The examples use
issue `148`; substitute the live issue number, area and a short kebab-case
slug.

### Branch and worktree

Name the branch `<type>/<area>-<slug>` per
[`setup-repo`](../../skills/setup-repo/SKILL.md)'s branch naming doctrine,
for example `fix/repo-runbook-mechanics`; no issue number in the branch, since
one branch may cover several issues or a slice. The worktree directory is the
branch with `/` replaced by `-`. The
standing `HARNESS.md` envelope covers branch and worktree creation without
extra authority. Removing a clean worktree also needs no extra authority;
dirty worktrees are refused and unmerged branches are kept.

```sh
provenant worktree create fix/repo-runbook-mechanics --base main
```

To also record the issue-to-branch link on GitHub, create the branch with
`gh issue develop` first, then point the helper at it:

```sh
gh issue develop 148 --name fix/repo-runbook-mechanics --base main
git fetch origin
git branch --track fix/repo-runbook-mechanics origin/fix/repo-runbook-mechanics
provenant worktree create fix/repo-runbook-mechanics
```

Use `provenant worktree list` to inspect worktrees and
`provenant worktree remove NAME` to remove an eligible one; its branch goes
too once merged. `--discard` also deletes an unmerged branch.

Then set the issue to `In progress` (commands under
[Project status](#project-status)).

### Commit and push

Reference each covered issue from a commit body with `Refs #N`. Never put a
closing keyword in a commit message; the pull request owns issue closure. Push
with an upstream so `gh pr create` finds the branch:

```sh
git push -u origin fix/repo-runbook-mechanics
```

A Fabric writer lane cannot write the shared Git `config`, so `-u` pushes but
cannot record the upstream. From a lane, push with `git push origin HEAD` and
pass `--head <branch>` to `gh pr create`.

### Pull request

Open the pull request against `main` and link the issue per the rule in
[Execute and review](#execute-and-review): `Closes #N` only when merge leaves
no user or external-action gate, otherwise `References #N` with the issue left
open. The body must follow the repository template
([`.github/pull_request_template.md`](../../.github/pull_request_template.md));
`gh pr create --body` bypasses the template, so fill a copy, with evidence rows
bound to the exact head SHA and the independent-review block included, and
pass it explicitly:

```sh
PR_BODY="$(mktemp)"
cp .github/pull_request_template.md "$PR_BODY"
# fill in every section, then:
gh pr create --base main \
  --title "docs(runbooks): document agent GitHub mechanics" \
  --body-file "$PR_BODY"
```

Set the issue to `In review` while exact-head checks and independent review
run, and `Awaiting user` once machine gates pass and only a user decision
remains.

### Project status

Project Status (project `2`, owner `mblauberg`) is the sole workflow state; no
effort or session document owns it. Ownership of each transition:

| Transition | Owner | When |
|---|---|---|
| `Backlog` to `Ready`, `Done` or unchanged | Triage: user, or an agent inside granted authority | The triage result is recorded in an issue comment |
| `Ready` to `In progress` | Implementing agent | Work on the accepted scope starts |
| `In progress` to `In review` | Implementing agent | The pull request, exact-head checks or independent review is active |
| `In review` to `Awaiting user` | Implementing agent | Machine gates pass; a required user decision remains |
| Any later state to `Done` | Merging agent or user; merge auto-closes a `Closes #N` issue | No gate remains; the terminal reason or integrated pull request is recorded |

Move an item with the project CLI. The Status field id is stable for this
project:

```sh
item=$(gh project item-list 2 --owner mblauberg --limit 200 --format json \
  --jq '.items[] | select(.content.number == 148) | .id')
gh project item-edit --project-id PVT_kwHOBiwkrc4BdU1c --id "$item" \
  --field-id PVTSSF_lAHOBiwkrc4BdU1czhX3Kn4 --single-select-option-id 5c9ddb06
```

Status option ids: `Backlog` `c764d63a`, `Ready` `a5ebd55b`, `In progress`
`5c9ddb06`, `In review` `27873f75`, `Awaiting user` `129da224`, `Done`
`93d6cd26`. If an id stops matching, re-derive it:

```sh
gh project field-list 2 --owner mblauberg --format json \
  --jq '.fields[] | select(.name == "Status")'
```

### Merge

Before queueing merge for a substantial software change, confirm its delivery receipt (`provenant workflow receipt show <id>`) is current. The pull request is the evidence: exact-head CI, required review and the repository merge.


Merge authority is repo-based. Review pressure follows the
[`HARNESS.md` Review ladder](../../HARNESS.md#review-ladder), including its
recorded-blocker fallback when cross-family review is unavailable. This
repository is a personal harness, not production: by user directive (2026-07-16), repository auto-merge is enabled
and agents merge directly. An agent merges a pull request once it has passed
its tier's review pressure (routine: chair plus objective checks; substantial+
uses the canonical ladder on the exact head) and
`ci-status` is green on the exact head, without waiting for the user. `gh pr
merge --auto` may be queued once those gates are met.

`ci-status` is the single required check on branch protection. It is the
aggregate job at the end of [`ci.yml`](../../.github/workflows/ci.yml): it
runs on `if: always()`, succeeds only when every job in its live `needs:` list
either succeeded or was skipped by the path filter, and fails closed on any
failure or cancellation. "CI is green" means exactly this one context; no
other check is required.

### Merge author identity

For an agent-initiated merge, derive the authenticated GitHub account's noreply
address at command time, pass it to `gh pr merge --author-email`, then verify
the generated merge commit's recorded author. This is the forward-only
repository practice tracked by #807.

```sh
pr=<number>
author_email="$(gh api user --jq '"\(.id)+\(.login)@users.noreply.github.com"')"
gh pr merge "$pr" --merge --author-email "$author_email"
merge_sha="$(gh pr view "$pr" --json mergeCommit --jq '.mergeCommit.oid')"
test "$(gh api "repos/{owner}/{repo}/commits/$merge_sha" --jq '.commit.author.email')" = "$author_email"
```

For an auto-merge, run the last two lines after GitHub has created the merge
commit. A mismatch is a publication check failure: stop before claiming the
merge is ready and record the generated author evidence.

For status-check rollup handling and false-green traps, follow the
[shell-pitfalls runbook](shell-pitfalls.md#shell-pitfalls).

The user review/merge gate applies only when the agent is stuck: split review
verdicts it cannot settle with primary-source evidence, an exhausted repair
budget, or a decision outside its granted authority. Standing user gates are:
forced branch deletion, deletion of a branch that is not merged, history
rewrites, credential or connector setup, pushes to shared branches outside
authorised merges, and risk-tier downgrades. Deleting a *merged* branch's own
local ref and worktree is carried by the merge itself, under
[post-merge pruning](../worktrees.md#ownership-and-cleanup).

Branch protection requires the head to be strictly up to date with `main`, so
concurrent pull requests still integrate as a serialised merge train: merge
one, update the next onto the new `main`, rerun the exact-head checks and
independent review (an update-merge is a new commit and invalidates prior
exact-head evidence), then merge it.

### Reproducing a CI failure locally

When a suite passes locally but fails in CI, the difference is usually developer
configuration rather than the code. Vary `HOME` before `PATH`: run the suite
with `HOME` pointed at an empty directory to find out whether local
configuration is masking the failure, and change one environment axis per run so
the result attributes to something.

Do not model CI with a minimal allowlist `PATH`. It manufactures failures that
have nothing to do with the real difference, and time then goes into those
instead of the actual cause. If a PATH reduction is genuinely needed, start from
the real `PATH` and subtract only the named executables under suspicion. Record
the matrix and which single axis changed the outcome.

### Dependabot patch-only auto-merge

One standing exception to tier review pressure:
[`dependabot-automerge.yml`](../../.github/workflows/dependabot-automerge.yml)
(issue #155) queues `gh pr merge --auto` unattended for Dependabot updates
when all of the following hold:

- the PR author is `dependabot[bot]` and the head branch lives in this
  repository (not a fork);
- `dependabot/fetch-metadata` reports the update type as
  `version-update:semver-patch` (minor and major updates wait for maintainer
  review); and
- Dependabot is configured for GitHub Actions and the Rust workspace's Cargo
  dependencies; the former npm package exclusions do not apply.

The queued merge still lands only after `ci-status` reports green; that gate is
the whole review pressure for these PRs.

### After merge

Afterwards:

1. For a software delivery, sync the primary checkout. After the merge
   commit's main-branch `ci-status` succeeds, record the accepted receipt
   with `provenant workflow receipt save`. Do not fabricate user approval.
2. Confirm the issue closed (`Closes #N`) or close it with its terminal reason
   recorded, and confirm Status is `Done`.
3. Prune the merged branch's artefacts. This is the last step of the merge, not
   a later sweep. [Post-merge pruning](../worktrees.md#ownership-and-cleanup) owns
   the standing authority, the method-specific proof gates and the deletion
   commands; run the complete repository, branch and merge gate first, and do
   not run the merge-commit and squash-merge paths as one sequence.

4. The user-authorised repository setting `delete_branch_on_merge=true`
   (enabled 2026-07-19) automatically deletes a merged pull request's remote
   head branch, so the remote ref is usually already gone by the time you prune;
   `git remote prune origin` clears the stale remote-tracking ref. This
   automatic merged-head cleanup needs no separate per-branch authority. Forced
   deletion, and deletion of an unmerged branch on either side, remains an
   explicit user gate.
