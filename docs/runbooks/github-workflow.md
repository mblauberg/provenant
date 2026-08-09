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
`harness`, `fabric`, `split-root`, `zizmor`)
either succeeded or was skipped by the path filter, and fails closed on any
failure or cancellation, including `detect-changes` itself. "CI is green"
means exactly this one context; no other check is required.

Read that check's state correctly. `gh pr view <n> --json statusCheckRollup`
leaves `.conclusion` as an empty string while a check is still running, so a
`jq` expression that only guards against `null` treats a running check as
finished and reports a false green. Filter on `.status` first, treating anything
other than `COMPLETED` as still running, and only then require every non-skipped
`.conclusion` to be `SUCCESS`. Merging on `.conclusion` alone merges unverified
heads.

The user review/merge gate applies only when the agent is stuck: split review
verdicts it cannot settle with primary-source evidence, an exhausted repair
budget, or a decision outside its granted authority. Standing user gates are:
forced branch deletion, deletion of a branch that is not merged, history
rewrites, credential or connector setup, pushes to shared branches outside
authorised merges, and risk-tier downgrades. Deleting a *merged* branch's own
local ref and worktree is carried by the merge itself, under
[post-merge pruning](../worktrees.md#post-merge-pruning).

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
- the dependency list does not include `@anthropic-ai/claude-agent-sdk`,
  `@anthropic-ai/claude-code` or `@openai/codex`.

These adapter dependencies are excluded even at patch level because an adapter
dependency update requires current runtime identity, handshake and capability
evidence. A green dependency bump alone does not prove that enabled activation
will remain conformant. The queued merge still lands only after `ci-status`
reports green; that gate is the whole review pressure for these PRs.

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
3. Prune the merged branch's artefacts. This is the last step of the merge, not
   a later sweep — see [Post-merge
   pruning](../worktrees.md#post-merge-pruning) for the standing authority and
   its limits. Run the complete repository, branch and merge gate before
   removing the clean worktree or deleting the local branch. **This repository
   squash-merges**, so the content gate — not the ancestry gate — is the one
   that applies here:

   ```sh
   # 1. Establish where you are. Both must match before anything mutates.
   test "$(git -C <primary-root> rev-parse --show-toplevel)" = "<primary-root>"
   test "$(git -C <primary-root> symbolic-ref --quiet --short HEAD)" = "<integration-branch>"

   # 2. Sync the integration branch.
   git -C <primary-root> fetch origin
   git -C <primary-root> merge --ff-only origin/<integration-branch>

   # 3. Prove the merge. A squash merge leaves no ancestry link, so
   #    `merge-base --is-ancestor` would refuse a branch that is fully merged.
   #    Prove content instead: both of these, and empty diff output, are the gate.
   gh pr view <n> --json state,headRefName,baseRefName
   git -C <primary-root> diff <integration-branch> <merged-branch> -- <paths the branch touched>

   # 4. Prune only that branch's artefacts.
   scripts/worktree remove <name> --repo <primary-root> --human-authorised
   git -C <primary-root> branch -D <merged-branch>   # -d cannot see a squash merge
   git -C <primary-root> worktree prune
   git -C <primary-root> remote prune origin
   ```

   Scope the step-3 diff to the paths the branch touched. Unscoped, it also
   reports everything `main` gained after the branch was cut, which reads as
   divergence when the branch is merely behind.

   `git cherry <upstream> <head>` is the other tool reached for here, and its
   two signs are not equally trustworthy. It compares patch ids, so a leading
   `-` proves an equivalent patch is already upstream and is sound grounds for
   closing or dropping that commit. A leading `+` proves only that no
   patch-identical commit was found, which is not the same as the work being
   outstanding: upstream work that was reshaped on the way in, including by this
   repository's own squash merges, still reports `+`. Close on `-`, and settle a
   `+` by reading the current upstream content and comparing behaviour.

   One repository-specific retention rule overrides this: a substantial software
   change's canonical `delivery-run` receipt directory must survive the merge
   (see [Merge](#merge)). Satisfy that first.

4. The user-authorised repository setting `delete_branch_on_merge=true`
   (enabled 2026-07-19) automatically deletes a merged pull request's remote
   head branch, so the remote ref is usually already gone by the time you prune;
   `git remote prune origin` clears the stale remote-tracking ref. This
   automatic merged-head cleanup needs no separate per-branch authority. Forced
   deletion, and deletion of an unmerged branch on either side, remains an
   explicit user gate.
