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
closing keyword.

Project `2` (owner `mblauberg`) holds Status. To set it:

```sh
item=$(gh project item-list 2 --owner mblauberg --limit 200 --format json \
  --jq '.items[] | select(.content.number == 148) | .id')
gh project item-edit --project-id PVT_kwHOBiwkrc4BdU1c --id "$item" \
  --field-id PVTSSF_lAHOBiwkrc4BdU1czhX3Kn4 --single-select-option-id 5c9ddb06
```

Option IDs: `Backlog` `c764d63a`, `Ready` `a5ebd55b`, `In progress`
`5c9ddb06`, `In review` `27873f75`, `Awaiting user` `129da224`, `Done`
`93d6cd26`. If one stops matching, re-derive it with `gh project field-list 2
--owner mblauberg --format json`.

## Branch, commit, pull request

Name the branch `<type>/<area>-<slug>`, at most 48 characters, `<type>` one of
`feat fix refactor perf test docs chore ci build`; no issue number and no run
vocabulary (batch, wave, lane, model name). Create it with
`provenant worktree create BRANCH --base main`; to record the issue link on
GitHub, create the branch first with `gh issue develop N --name BRANCH`.
`worktree remove NAME` refuses dirty worktrees; `--discard` also deletes an
unmerged branch.

Push with `git push -u origin BRANCH`; a writer pushes its own branch unless
`lanes.publish=false`, when the `deliver` template's host delivery does it.

`gh pr create --body` bypasses
[the template](../../.github/pull_request_template.md): fill a copy (evidence
rows bound to the exact head, the independent-review block) and pass it with
`--body-file`, or build it from recorded attempts with
`provenant pr open --from-attempt A --review B --summary-file summary.md --issue N --dry-run`.
Complete rows marked `pending` before asking for review.

A stack is a linear chain of dependent slices: the bottom PR targets `main`,
each later PR the branch below, each with its own issue link (`gh stack --help`).
Rebase and force-push with lease only your own unmerged branches, then recheck
heads and review evidence. Land bottom-first with `provenant land`.

## Merge

Agents merge directly: once a pull request has its tier's planned review (the
[`HARNESS.md` review ladder](../../HARNESS.md#review-ladder)) and `ci-status`
is green on the exact head, merge without waiting for the user. A change that
only repairs CI or restores a red `main` merges on green `ci-status` alone; one
that skips or weakens a check takes its normal review. Before merging a
substantial change, confirm its receipt is current
(`provenant workflow receipt show ID`).

`ci-status` is the single required check: the aggregate job at the end of
[`ci.yml`](../../.github/workflows/ci.yml). `Protect main` does not require
heads up to date with `main`, so land each pull request on its own green head and
merge `main` into a branch only to resolve a conflict (it costs a CI run and
moves the reviewed head). A combined break on `main` is fixed forward. Standing
user gates: credential or connector setup, and rewrites of or pushes to shared
branches outside authorised merges.

### Land

```sh
provenant land 148 149              # ordered batch
provenant land 148 --shadow         # gate and record without publishing
provenant land 148 --wait=30m       # single-PR path
provenant land --tier substantial --explain --json
```

By default (`integration = "batch"`) `land` builds one merge commit per PR onto
the fetched target, gates the exact candidate and publishes it
([ADR 0037](../adr/0037-batched-exact-candidate-landing.md)). `--wait`,
`--head` or `integration = "single"` select the single-PR path, which waits for
CI (up to four hours) and merges over REST at the verified head. `--explain
--json` shows the effective policy and its sources; settings live under
`[domains.pr.land]` (`config schema` lists the keys).

Sharp edges:

- Changing `candidate_ref_prefix` needs a matching CI trigger (the bundled one
  covers `land/**`). A command gate runs without `GH_TOKEN`, `GITHUB_TOKEN` and
  `GH_ENTERPRISE_TOKEN` unless `gate.env_passthrough` names them.
- Approval. `approval = "forge"` (default) needs an approving forge review at
  the head; `receipt` takes an accepted delivery receipt instead. This
  repository's host uses `receipt`: save it before landing, with a `pr` subject
  naming `number` and the reviewed `head`. A merge-in from the target, or a
  rebase with an unchanged delta, keeps the review; an unchanged head retargeted
  past the branch below does not. Any other change needs a receipt whose
  `subject` adds `delta_from` (the reviewed head), or
  `land N --accept-delta REASON`, which warns and records the reason.
  `approval_tier = "auto"` needs no review for changes touching only tests,
  docs or review-exempt paths.
- Single-PR exits: 124 is a CI timeout (nothing merged), 5 is `pending`
  (rerun with `--wait`). `CI_CONFLICT`: the PR conflicts with its base, so CI
  never starts; rebase or merge, then push. `MERGE_STATE_UNKNOWN`: rerun `land`.
- Cleanup after merge (fast-forward base, delete build output, worktree and
  local branch) runs only when the local tip is exactly the merged head. A
  refused step reports `cleanup_pending` and the merge still exits 0; fix the
  cause and rerun.

#### Recover an interrupted landing

The lander writes a marker under `landing/pending/` before publishing; an
ambiguous outcome (timeout, 5xx) keeps it and the error names it. Never delete
it to force a retry.

Restore forge access, check `git merge-base --is-ancestor CANDIDATE origin/TARGET`
after fetching and `gh pr view N --json state,headRefOid,mergeCommit` per PR,
then after `publish_settle_seconds` rerun `provenant land N` without `--shadow`:
recovery observes the outcome, finishes receipts and deletes the marker last.

REST merges use the authenticated GitHub account's commit email, not local
`user.email`: configure it in GitHub first and check the merge commit's author;
an unexpected author is a publication failure, so stop and record it.

### After merge

Sync the primary checkout. Unless the receipt already stood as the landing
approval, record it with `provenant workflow receipt save` after `ci-status`
on `main` succeeds; never fabricate user approval. Confirm the issue closed and
Status is `Done`. Prune the branch's artefacts per
[`docs/worktrees.md`](../worktrees.md#ownership-and-cleanup);
`git remote prune origin` clears the stale tracking ref. Deleting an unmerged
branch on either side is a user gate.

Dependabot patch and minor bumps that change only manifests and lockfiles
auto-merge on green `ci-status`; anything else takes a `routine` reviewer.

## Scheduled tracker upkeep

On one scheduling host per repository, opt in with
`[domains.steward.repositories.tracker]` (`directory`, `cadence_seconds`);
`provenant tick` then launches the `steward` graph when due.
