# GitHub work-item workflow
Owner: repository maintainers
Anchor: [`.github/ISSUE_TEMPLATE/work-item.yml`](../../.github/ISSUE_TEMPLATE/work-item.yml); [`landing::driver::run`](../../tools/front-door/src/pr/landing/driver.rs)

Project Status is the sole workflow state; labels describe content or gates,
never progress. [`.github/labels.yml`](../../.github/labels.yml) is synced with
pruning, so a label missing from it is deleted on the next sync: never
hand-create one. Issues come from the forms under `.github/ISSUE_TEMPLATE/`.
Filing, triage and sub-issue mechanics are in the
[`tracker`](../../skills/tracker/SKILL.md) skill; specs in
[`scope`](../../skills/scope/SKILL.md).

## Status

| Status | Meaning |
|---|---|
| Backlog | Untriaged or deferred. |
| Ready | Accepted with bounded scope, authority and acceptance evidence. |
| In progress | An owner is executing the accepted work. |
| In review | The pull request, checks or independent review is active. |
| Awaiting user | A user decision remains, including explicit acceptance for `terminal` work. |
| Done | Integrated, or closed with its terminal reason recorded. |

The implementing agent moves `Ready` to `In progress` to `In review` and, when
machine gates pass but a user decision remains, to `Awaiting user`. The
project's workflows set `Done` when the issue closes or its pull request
merges; setting `Done` closes an open issue, so never set it on work that must
stay open. Link a pull request with `Closes #N` only when merge leaves no user
or external-action gate; otherwise `References #N`. Commits never carry a
closing keyword. Find the project item and the current Status options with
`gh project item-list` and `gh project field-list` before editing one.

## Branch, commit, pull request

Name the branch `<type>/<area>-<slug>` without an issue number or run
vocabulary. `provenant worktree create BRANCH --base main` makes the worktree
(see [worktrees](../worktrees.md)); use `provenant worktree locate BRANCH` (or a
lane, attempt or PR number) to find its checkout instead of parsing raw
`git worktree list` output. `provenant worktree remove BRANCH` records the
removed tip and prunes the local branch once it is merged. To record the issue
link on GitHub, create the branch first with `gh issue develop N --name BRANCH`.
Lane publishing is in [lane delivery](lane-lifecycle.md#delivery).

`gh pr create --body` bypasses [the template](../../.github/pull_request_template.md):
fill a copy (evidence rows bound to the exact head, the independent-review block)
and pass it with `--body-file`, or build it from recorded attempts with
`provenant pr open --from-attempt TARGET --summary-file summary.md --issue N --dry-run`.
Complete rows marked `pending` before asking for review. A lane opens its own
pull request with `gh pr create --body-file` because its attempt is still
running and `pr open` refuses it; `pr open` is for the chair after the attempt
settles.

A stack is a linear chain of dependent slices: the bottom PR targets `main`,
each later PR the branch below, each with its own issue link (`gh stack --help`).
Rebase and force-push with lease only your own unmerged branches, then recheck
heads and review evidence. Land bottom-first with `provenant land`.

## Repair a pull request

`provenant run implement --pr N` repairs a pull request from its CI failures.
The host resolves the PR's head branch and base through the forge, locates or
creates the head branch's worktree, and appends the failing checks — each name
and a bounded log tail read through the one CI observer — to the lane's brief.
Re-run it after a push or a check finishes to refresh the evidence; use
`provenant ci failures N` first to read the same failures without launching a
lane.

`--base` names the pull request's merge target; it never resets the branch. An
existing branch behind its base warns `BRANCH_BEHIND_BASE` and proceeds at its
current tip, so re-running a repair keeps the branch's commits. Merge the base
into the branch before landing when the warning appears. A `--base` with no merge
base with the branch refuses: pass `--worktree` to use the existing checkout, or
merge or rebase the branch onto the intended base before launching again.

## Merge

Agents merge directly: once a pull request has the lens review its risk
requires (`provenant run review`) and `ci-status` is green on the exact head,
merge without waiting for the user. CI and test-speed fixes, and changes that
restore a red `main`, merge on green `ci-status` alone; changes that skip or
weaken a check or alter a real boundary take their normal review. Before
merging a substantial change, confirm its receipt is current
(`provenant workflow receipt show ID`).

Repository admins may bypass `Protect main` for pull-request merges only;
direct pushes still need CI. The `gh pr merge N --merge --admin` escape hatch
is for an earlier green head of the same pull request whose later delta is
docs, skills or release notes only (`git diff --stat GREEN NEW`); never for a
code delta, a red check or an unreviewed change. `provenant land` does not
invoke that escape hatch by default. If a ruleset demands a review the
authenticated owner cannot give, set
`domains.pr.land.admin_publication = "when_review_unavailable"` in the owner's
**host** config: after land's own gate passed (green CI on the exact
candidate and independent review evidence), a review-requirement 405 is
retried with the same admin API `--admin` uses. Any other layer setting is
warned (`SETTING_LAYER_IGNORED`) and ignored; red CI, a missing review receipt
and every other 405 cause are never bypassed. The receipt records the grant
and its source. This changes how the owner's authority is exercised, not who
holds it.

An admin merge under the CI-bypass grant is not an exemption from deferred
coverage: before exercising it, run the deferred groups for every touched crate
with `ci:full` (or `dispatch tier=full`) and wait for that run. The grant answers
a review-requirement refusal only; it never answers a red check or a missing
full-tier run for a code delta.

`ci-status` is the single required check: the aggregate job at the end of
[`ci.yml`](../../.github/workflows/ci.yml). `Protect main` does not require
heads up to date with `main`, so land each pull request on its own green head and
merge `main` into a branch only to resolve a conflict (it costs a CI run and
moves the reviewed head). A combined break on `main` is fixed forward. Standing
user gates: credential or connector setup, and rewrites of or pushes to shared
branches outside authorised merges.

### Land

`provenant land --help` lists options and `provenant land --explain` shows the
effective policy and its sources (`domains.pr.land.*`). The landing owner gates
the exact candidate ([ADR 0037](../adr/0037-batched-exact-candidate-landing.md)).
Review must be independent of the writer and cover the exact head; a changed
head needs fresh evidence.

- `LAND_PENDING` and `LAND_BUSY` exit as `pending`: rerun the same `land`.
  `LAND_CONFLICT` means the PR conflicts with the target: merge the base into the branch, then
  push. `LAND_PUBLICATION_UNKNOWN`: see the recovery below.
- A killed land leaves its candidate ref until the pull request is merged or
  closed; the next land collects it. Changing `domains.pr.land.candidate_ref_prefix`
  needs a matching CI trigger (the bundled one covers `land/**`).
- A command gate runs without `GH_TOKEN`, `GITHUB_TOKEN` and
  `GH_ENTERPRISE_TOKEN` unless `domains.pr.land.gate.env_passthrough` names them.
- Cleanup after merge (fast-forward base, delete build output, worktree and
  local branch) runs only when the local tip is exactly the merged head. A
  refused step reports `cleanup_pending` and the merge still exits 0; fix the
  cause and rerun.

#### Recover an interrupted landing

The lander writes a marker under `landing/pending/` before publishing; an
ambiguous outcome (timeout, 5xx) keeps it and the error names it. Never delete
it to force a retry.

Restore forge access, fetch, check `git merge-base --is-ancestor CANDIDATE origin/TARGET`
and `gh pr view N --json state,headRefOid,mergeCommit` per PR, then rerun
`provenant land N` without `--shadow`: recovery observes the outcome, finishes
receipts and deletes the marker last. It never sleeps, so a marker younger than
`domains.pr.land.publish_settle_seconds` that it cannot yet prove stays for the
next run.

REST merges use the authenticated GitHub account's commit email, not local
`user.email`: configure it in GitHub first and check the merge commit's author;
an unexpected author is a publication failure, so stop and record it.

### After merge

Sync the primary checkout. Unless the receipt already stood as the landing
approval, record it with `provenant workflow receipt save` after `ci-status`
on `main` succeeds; never fabricate user approval. Confirm the issue closed and
Status is `Done`. Prune the branch's artefacts per
[`docs/worktrees.md`](../worktrees.md#ownership-and-cleanup);
`git remote prune origin` clears the stale tracking ref.

Dependabot patch and minor bumps that change only manifests and lockfiles
auto-merge on green `ci-status`; anything else takes a `routine` reviewer.

## Scheduled tracker upkeep

On one scheduling host per repository, set
`domains.steward.repositories.tracker.directory` and
`domains.steward.repositories.tracker.cadence_seconds`; `provenant tick` then launches the `steward` graph when due.
