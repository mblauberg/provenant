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
| Awaiting user | Machine work is ready but a user decision or acceptance remains. |
| Done | The item is integrated, or closed with its terminal reason recorded. |

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
   choice, final acceptance or release decision moves to `Awaiting user`; an
   agent never infers it. User-originated items follow the same evidence and
   scope checks.

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
prose — GitHub renders Mermaid in issues, pull requests and comments. Flows and
roundtrips, before/after refactors, state machines, decision trees for open
decisions, and cross-issue dependencies are strong candidates; a table is the
right visual for tabular data. Prefer one wherever it saves reviewer attention,
not only when prose would be hard. Skip it only when the change is simple enough
that prose is already clear at a glance. Keep every diagram small, legible and
captioned — a diagram that does not save the reader time is noise, so cut it.

## Mechanics

This runbook is provenant-local repository process, not harness doctrine. The
worktree authority it relies on (the standing envelope in `HARNESS.md` and the
helper contract in [`docs/worktrees.md`](../worktrees.md)) is harness doctrine
and applies wherever the harness is loaded.

The loop below takes an accepted issue from `Ready` to `Done`. The examples use
issue `148`; substitute the live issue number and a short kebab-case slug.

### Branch and worktree

Name the branch `issue-N-slug`, for example `issue-148-runbook-mechanics`.
Create the linked branch, then a worktree on it. `gh issue develop` records the
issue-to-branch link on GitHub; the helper enforces the shared-worktree
contract, and its authorisation flags attest that the standing `HARNESS.md`
envelope or a direct user instruction covers the operation:

```sh
gh issue develop 148 --name issue-148-runbook-mechanics --base main
git fetch origin
scripts/worktree create impl-148 --human-authorised \
  --existing-branch issue-148-runbook-mechanics
```

When the GitHub-side branch link is not needed, create the branch and worktree
in one step:

```sh
scripts/worktree create impl-148 --human-authorised \
  --new-branch issue-148-runbook-mechanics --branch-authorised \
  --start-point main
```

Then set the issue to `In progress` (commands under
[Project status](#project-status)).

### Commit and push

Reference the issue from every commit body with `Refs #N`. Never put a closing
keyword in a commit message; the pull request owns issue closure. Push with an
upstream so `gh pr create` finds the branch:

```sh
git push -u origin issue-148-runbook-mechanics
```

### Pull request

Open the pull request against `main` and link the issue per the rule in
[Execute and review](#execute-and-review): `Closes #N` only when merge leaves
no user or external-action gate, otherwise `References #N` with the issue left
open. The body must follow the repository template
([`.github/pull_request_template.md`](../../.github/pull_request_template.md));
`gh pr create --body` bypasses the template, so fill a copy — evidence rows
bound to the exact head SHA and the independent-review block included — and
pass it explicitly:

```sh
cp .github/pull_request_template.md /tmp/pr-body.md
# fill in every section, then:
gh pr create --base main \
  --title "docs(runbooks): document agent GitHub mechanics" \
  --body-file /tmp/pr-body.md
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
| `In review` to `Awaiting user` | Implementing agent | Machine gates pass; a user decision or acceptance remains |
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

Before queueing merge for a substantial software change, validate its one
canonical `delivery-run` receipt in `awaiting_acceptance` and retain the entire
ignored run directory. Do not remove the worktree or discard that directory
after GitHub merges it. This is a receipt-continuity gate, not user acceptance
or promotion authority. When post-merge GitHub binding is in scope, its already
approved Authority V2 envelope must allowlist `api.github.com` tool egress and
grant use-without-disclosure of the `github-cli-auth` secret reference; the
binder never infers those grants from the operator's login.

Merge authority is repo-based. Review pressure follows [`HARNESS.md`](../../HARNESS.md):
targeted lenses plus the other-primary leg are load-bearing from substantial up;
crucial work uses a distinct family when available, and terminal work adds
stronger targeted/adversarial pressure with any skip recorded. This repository is a personal harness, not
production: by user directive (2026-07-16), repository auto-merge is enabled
and agents merge directly. An agent merges a pull request once it has passed
its tier's review pressure (routine: chair plus objective checks; substantial+
uses the canonical ladder on the exact head) and
`ci-status` is green on the exact head, without waiting for the user. `gh pr
merge --auto` may be queued once those gates are met.

`ci-status` is the single required check on branch protection. It is the
aggregate job at the end of [`ci.yml`](../../.github/workflows/ci.yml): it
runs on `if: always()`, succeeds only when every needed job (`detect-changes`,
`harness`, `fabric`, `review-portal-supervisor`, `console`, `herdr`, `zizmor`)
either succeeded or was skipped by the path filter, and fails closed on any
failure or cancellation, including `detect-changes` itself. "CI is green"
means exactly this one context; no other check is required.

The user review/merge gate applies only when the agent is stuck: split review
verdicts it cannot settle with primary-source evidence, an exhausted repair
budget, or a decision outside its granted authority. Standing user gates are
unchanged: manual or forced branch deletion outside the repository's automatic
merged-head cleanup, history rewrites, credential or connector setup, pushes to
shared branches outside authorised merges, and risk-tier downgrades.

Branch protection requires the head to be strictly up to date with `main`, so
concurrent pull requests still integrate as a serialised merge train: merge
one, update the next onto the new `main`, rerun the exact-head checks and
independent review (an update-merge is a new commit and invalidates prior
exact-head evidence), then merge it.

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
- the dependency list does not include `@anthropic-ai/claude-agent-sdk`.

The SDK is excluded even at patch level (issue #195):
[`config/adapter-compatibility.yaml`](../../config/adapter-compatibility.yaml)
pins it by version, artifact, lock integrity, entrypoint and schema, and CI's
portable-fixtures mode bypasses those pins, so a green SDK bump can still
leave enabled Claude activation fail-closed until the pins are refreshed
alongside it. The queued merge still lands only after `ci-status` reports
green; that gate is the whole review pressure for these PRs.

### After merge

Afterwards:

1. For a software delivery, sync the primary checkout and copy the retained run
   directory into the same workspace-relative `.agent-run/<id>/` location.
   After the merge commit's main-branch `ci-status` succeeds, bind the exact
   merge, PR and review evidence while the receipt remains
   `awaiting_acceptance`:

   ```sh
   skills/implement/scripts/bind_merged_delivery.py \
     .agent-run/<id>/RUN.json --workspace-root "$PWD" \
     --repository owner/repository --pr-number <number> \
     --review-artifact <targeted-review.json> \
     --review-artifact <other-primary-review.json>
   skills/deliver/scripts/validate_delivery.py \
     .agent-run/<id>/RUN.json --workspace-root "$PWD" --verify-hashes
   ```

   The binder reads the merged PR and exact merge-commit `ci-status` from the
   authenticated GitHub API; it does not accept caller-authored success flags.
   Review arguments are pre-existing typed exact-head artifacts, not verdicts
   created by the binder. It holds an exclusive receipt lock, stages the whole
   update and fails if the reviewed and merged trees differ. The source artifact
   records the exact full-width native Git commit and resolved tree without a
   second archive or per-file hash. Git evidence reads discard inherited
   repository, object and config routing, replacements and grafts, and never
   lazy-fetch missing promisor objects; local PR, CI and review JSON remain
   SHA-256 verified. Do not
   request acceptance or promotion authority until validation passes. Explicit
   user acceptance advances this same receipt to `accepted` and then
   `awaiting_release`; release binds the same exact artifact identity and never
   reconstructs it.
2. Confirm the issue closed (`Closes #N`) or close it with its terminal reason
   recorded, and confirm Status is `Done`.
3. After syncing the main checkout, keep the fabric dist warm so
   `scripts/agent-fabric` never falls back to the slow tsx loader path
   (no-op when the dist is fresh; see [Keep the CLI dist
   warm](agent-fabric-operations.md#keep-the-cli-dist-warm)):

   ```sh
   scripts/agent-fabric-warm
   ```

4. Prune the merged branch's artefacts. This is the last step of the merge, not
   a later sweep — see [Post-merge
   pruning](../worktrees.md#post-merge-pruning) for the standing authority and
   its limits. Confirm the merge landed, then remove the worktree once
   `git status` in it is clean and no live agent, pane or unconsumed handoff
   remains, and delete the merged local branch:

   ```sh
   git branch --merged main | grep issue-148-runbook-mechanics
   scripts/worktree remove impl-148 --human-authorised
   git branch -d issue-148-runbook-mechanics
   git worktree prune
   ```

   One repository-specific retention rule overrides this: a substantial software
   change's canonical `delivery-run` receipt directory must survive the merge
   (see [Merge](#merge)). Satisfy that first.

5. The user-authorised repository setting `delete_branch_on_merge=true`
   (enabled 2026-07-19) automatically deletes a merged pull request's remote
   head branch, so the remote ref is usually already gone by the time you prune;
   `git remote prune origin` clears the stale remote-tracking ref. This
   automatic merged-head cleanup needs no separate per-branch authority. Forced
   deletion, and deletion of an unmerged branch on either side, remains an
   explicit user gate.
