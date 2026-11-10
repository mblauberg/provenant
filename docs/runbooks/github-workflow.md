# GitHub work-item workflow
Owner: repository maintainers
Anchor: [`.github/ISSUE_TEMPLATE/work-item.yml`](../../.github/ISSUE_TEMPLATE/work-item.yml); [`landing::driver::run`](../../tools/front-door/src/pr/landing/driver.rs)

Use this process for user- and agent-originated repository work. Project
Status is the sole workflow state; labels describe content or gates, not
progress. The label taxonomy is declarative in
[`.github/labels.yml`](../../.github/labels.yml) and synced with pruning, so a
label missing from that file is deleted on the next sync; never hand-create
one. Issues come from the forms under `.github/ISSUE_TEMPLATE/`; blank issues
are disabled.

To deliver through Fabric, save `delivery.toml` for the `deliver` template
([example](presets.md)), run
`provenant run --use deliver --file delivery.toml --explain`, inspect its
authority and gates, then submit without `--explain`. Use `batch` for parallel
slices merged into one integration branch; see [templates](presets.md).

## Scheduled tracker upkeep

Opt in on one scheduling host per repository:

```toml
[domains.steward.repositories.tracker]
directory = "/absolute/path/to/checkout"
cadence_seconds = 86400
```

`provenant tick` then launches the bundled `steward` graph when due, one sweep
per project at a time; without the section it reports `steward=idle`. Proposal
mode only posts marked duplicate comments on the newer issue; it never closes
issues or applies parent links. See the [tracker skill](../../skills/tracker/SKILL.md)
for inspection, recovery and disabling.

## Statuses

| Status | Meaning |
|---|---|
| Backlog | Untriaged or explicitly deferred. |
| Ready | Accepted with bounded scope, authority and acceptance evidence. |
| In progress | An owner is executing the accepted work. |
| In review | The pull request, checks or independent review is active. |
| Awaiting user | A user decision remains, including explicit acceptance for terminal work or approved scope. |
| Done | The item is integrated, or closed with its terminal reason recorded. |

Objective checks and tier-required review meet delivery acceptance; the user
may still reject the outcome. Explicit user acceptance is required only for
`terminal` work or when the approved scope asks for it. A project may name its
landing gate as acceptance; that grants no merge or promotion authority.

## Change spec

The issue body owns the ephemeral change spec. Its authority ends with the
delivering PR, and any rule it settles moves to its durable home in that PR.
Write it with the [`scope`](../../skills/scope/SKILL.md) skill: the owner
approves one digest, and implementers cannot change the acceptance criteria.
At acceptance the reviewer checks spec drift and records any mismatch; this is
judgement, not an automatic gate. `scripts/check-scratch-paths` warns about
possible scratch citations in tracked Markdown and issue templates.

## Agent filing

Filing follows the [`tracker`](../../skills/tracker/SKILL.md) skill: recorded
duplicate searches, one issue per outcome, `source:agent`, the `Filed-by:`
trailer and evidence for every agent-filed defect. Issues are Epic, Feature,
Task or Bug. Only architecture becomes an ADR; other settled calls go to the
doc they change, and owner-only questions go through `provenant ask`. This
repository's advisory Ready cap is about three times the concurrent writer
lanes; at the cap, deliver or reconcile existing work before filing more.

Re-check an issue when a referenced PR merges, a dependency closes or evidence
moves. Age, silence and `updatedAt` never establish staleness or completion.

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
3. Agents triage only inside their granted authority. A spec, one-way-door
   choice, required explicit acceptance or release decision moves to
   `Awaiting user`; an agent never infers it.

### Native sub-issues

The parent issue owns change scope, story and remaining gates; native child
issues own independently deliverable slices. Link a child with
`gh issue create --parent N` (and `--blocked-by` for ordering) or, with an older
`gh`, the `sub_issues` API; never with a checklist or title. Linking needs
explicit external-write authority; without it, return the proposed relation.

```sh
child_id=$(gh api repos/OWNER/REPO/issues/CHILD_NUMBER --jq .id)
gh api --method POST repos/OWNER/REPO/issues/PARENT_NUMBER/sub_issues \
  -F sub_issue_id="$child_id"
```

A parent URL in a child body is navigation only. Closing the parent stays gated
until its children and every parent-level gate are handled.

## Execute and review

1. Set `In progress` when an owner starts the accepted scope.
2. Link the pull request with `Closes #N` only when merge leaves no user or
   external-action gate; otherwise use `References #N` and keep the issue open.
3. Set `In review` while exact-head checks and independent review run. A new
   head needs CI and a review of its delta.
4. If machine gates pass but a user gate remains, set `Awaiting user`. The
   user's decision comment names the choice, artefact or exact head, its
   evidence and every remaining gate.
5. Set `Done` only when no gate remains and the terminal reason or integrated
   pull request is recorded.

Use a small, captioned Mermaid diagram in an issue or PR when it saves the
reviewer time: flows, before-and-after refactors, state machines and
cross-issue dependencies. Use a table for tabular data.

## Mechanics

This runbook is Provenant-local process. The worktree authority it relies on,
the standing envelope in `HARNESS.md` and the helper contract in
[`docs/worktrees.md`](../worktrees.md), is harness doctrine. The examples use
issue `148`.

### Branch and worktree

Name the branch `<type>/<area>-<slug>`, at most 48 characters, where `<type>` is
`feat`, `fix`, `refactor`, `perf`, `test`, `docs`, `chore`, `ci` or `build`.
Use no issue number and no run vocabulary such as batch, wave, lane or model
name. The standing `HARNESS.md` envelope covers branch and worktree creation.

```sh
provenant worktree create fix/repo-runbook-mechanics --base main
```

To record the issue-to-branch link on GitHub, create the branch with
`gh issue develop 148 --name BRANCH --base main`, fetch it, create a tracking
branch and then run `provenant worktree create BRANCH`. `provenant worktree
list` inspects worktrees; `provenant worktree remove NAME` removes an eligible
one and its merged branch, and `--discard` also deletes an unmerged branch for
superseded work. Dirty worktrees are refused.

### Commit and push

Reference each covered issue from a commit body with `Refs #N`; never put a
closing keyword in a commit, because the pull request owns closure. Push with
an upstream: `git push -u origin BRANCH`. A Fabric writer with open network can
push its own branch and run `gh` unless `lanes.publish=false` (see
[lane lifecycle](lane-lifecycle.md)); otherwise the `deliver` template's host
delivery pushes the verified commit and opens the pull request.

### Pull request

Open the pull request against `main`, or its stack base, and link the issue as
in [Execute and review](#execute-and-review). `gh pr create --body` bypasses
the [template](../../.github/pull_request_template.md), so fill a copy, with
evidence rows bound to the exact head and the independent-review block, and
pass it with `--body-file`.

To build the body from recorded attempts instead:

```sh
provenant pr open --from-attempt <writer-attempt> --review <review-attempt> \
  --summary-file summary.md --issue 148 --dry-run
```

The summary file's first line is the title and the rest is the Summary. The
writer attempt must be local, `succeeded` or `review_required`, with its head
on the remote; read-only reviews supply the reviewer rows. Complete the rows
marked `pending` before asking for review, then drop `--dry-run`.
`provenant pr status 148 149` prints one line per PR, such as
`#148 OPEN <head> checks=3/4 pending=1 merge=blocked review=none`;
`--wait=5m` blocks until a snapshot changes and exits 124 if none does.
`pr status`, `pr watch` and `land` read CI through one observer per
repository (ADR 0037), so concurrent waiters on a head share one read per poll
and a rate limit pauses them all until its named reset.

### Stacked pull requests

Stack dependent slices of one outcome that merit separate review: a shared seam
and its consumers, or a large refactor in reviewable steps. Independent changes
get separate PRs off `main`, and a slice that cannot pass CI alone is not
stacked. GitHub stacks are a linear chain in one repository: the bottom PR
targets `main` and each later PR the branch below. Each PR carries its own
issue link and is reviewed against the branch below.

With `gh extension install github/gh-stack`, use `gh stack init`, `add BRANCH`,
`submit`, `view`, `rebase [--upstack|--downstack]`, `sync` and `unstack`. A
completed stack cannot be extended. A writer may rebase and force-push with
lease its own unmerged branches, a whole stack included, never `main` or another
writer's branch. Recheck changed heads and their review evidence afterwards.

Required checks apply to every PR in a stack. Land bottom-first with
`provenant land`; an upper PR waits for its lower members, and after
retargeting its new head needs its own CI and review. See GitHub's
[stacked PRs overview](https://docs.github.com/en/pull-requests/get-started/about-stacked-prs)
and [CLI reference](https://docs.github.com/en/pull-requests/reference/stacked-prs-cli-commands).

### Project status

Project `2` (owner `mblauberg`) holds Status:

| Transition | Owner | When |
|---|---|---|
| `Backlog` to `Ready`, `Done` or unchanged | Triage: user, or an agent inside granted authority | The triage result is recorded in an issue comment |
| `Ready` to `In progress` | Implementing agent | Work on the accepted scope starts |
| `In progress` to `In review` | Implementing agent | The pull request, exact-head checks or independent review is active |
| `In review` to `Awaiting user` | Implementing agent | Machine gates pass; a required user decision remains |
| Any later state to `Done` | Project workflow, when the issue closes or its pull request merges | No gate remains; the terminal reason or integrated pull request is recorded |

The project's workflows also move items when a pull request links, merges or an
issue closes, and setting `Done` closes an open issue, so never set `Done` on
work that should stay open.

```sh
item=$(gh project item-list 2 --owner mblauberg --limit 200 --format json \
  --jq '.items[] | select(.content.number == 148) | .id')
gh project item-edit --project-id PVT_kwHOBiwkrc4BdU1c --id "$item" \
  --field-id PVTSSF_lAHOBiwkrc4BdU1czhX3Kn4 --single-select-option-id 5c9ddb06
```

Status option IDs: `Backlog` `c764d63a`, `Ready` `a5ebd55b`, `In progress`
`5c9ddb06`, `In review` `27873f75`, `Awaiting user` `129da224`, `Done`
`93d6cd26`. If an ID stops matching, re-derive it with
`gh project field-list 2 --owner mblauberg --format json --jq '.fields[] | select(.name == "Status")'`.

## Merge

Merge authority is repository-based. By user directive, auto-merge is enabled
and agents merge directly: once a pull request has its tier's planned
independent review under the
[`HARNESS.md` review ladder](../../HARNESS.md#review-ladder) and `ci-status` is
green on the exact head (or on the candidate, for batch landing), an agent
merges without waiting for the user. Forge-required checks stay mandatory.
Before merging a substantial software change, confirm its delivery receipt
(`provenant workflow receipt show <id>`) is current.

A change that only repairs CI or restores a red `main` is a hotfix: an agent
merges it as soon as `ci-status` is green on its head, without the planned
review, so one breakage does not stall every other pull request. A change that
skips or weakens a check, or touches a path on a crucial review floor, is not a
hotfix and takes its normal review.

`ci-status` is the single required check: the aggregate job at the end of
[`ci.yml`](../../.github/workflows/ci.yml) succeeds only when every job it needs
succeeded or was path-skipped. "CI is green" means exactly that context. The
`Protect main` ruleset does not require heads to be up to date with `main`, so
land each pull request on its own green head. Do not merge `main` into a branch
to land it; update a branch only to resolve a conflict. Each update costs a CI
run and moves the reviewed head. Batch landing (`provenant land A B`) tests the
combination when several pull requests overlap. A combined break that shows on
`main` is fixed forward as a hotfix. For status-check rollup traps, see
[shell pitfalls](shell-pitfalls.md#shell-pitfalls).

The user review or merge gate applies only when an agent is stuck: split
verdicts it cannot settle with primary evidence, or a decision outside its
authority. Standing user gates are credential or connector setup, rewrites of
or pushes to shared branches outside authorised merges. A chair may lower risk
outside protected boundaries with a recorded reason; boundary floors remain
binding even with owner approval.

### Land

```sh
provenant land 148 149              # ordered batch
provenant land 148 --batch          # batch of one
provenant land 148 149 --shadow     # gate and record without publishing
provenant land 148 --wait=30m       # single-PR path
provenant land --tier substantial --explain --json
```

With the default `integration = "batch"`, any `land` builds one merge commit per
PR onto the fetched target, gates the exact candidate and publishes it; see
[ADR 0037](../adr/0037-batched-exact-candidate-landing.md) for the design and
its guarantees. `--wait`, `--head` or `integration = "single"` select the
single-PR path. `--explain --json` reports the effective policy and its
sources. Settings live under `[domains.pr.land]`, with `projects.<project>` and
`tiers.<tier>` overrides; the
[generated reference](../reference/config-keys-all.md#host-land-settings) lists
each key. The bundled CI trigger covers `land/**`, so changing
`candidate_ref_prefix` needs a matching CI trigger. `forge_queue` is rejected
at runtime. A command gate runs without `GH_TOKEN`, `GITHUB_TOKEN` and
`GH_ENTERPRISE_TOKEN` unless `gate.env_passthrough` names them, though Git
credential helpers and keychains stay available. `post_merge.revert_forward`
needs `post_merge.bisect` to name a culprit. Unknown cost never refuses;
`budget.cap_micros` refuses only priced spend. For a migration from a bespoke lander, see
[landing migration](landing-migration.md).

`approval` names what stands as a pull request's approval. `forge`, the
default, needs an approving forge review at the head. `receipt` takes an
accepted delivery receipt for the pull request instead; this repository's
reviews are such receipts, so its host sets
`domains.pr.land.projects.<project>.approval = "receipt"`. Save the receipt
before landing, with a `pr` subject naming the pull request `number` and the
reviewed `head`. The save reads the rest from the forge: the repository, and
the base, which is the head's merge base with the branch the pull request
merges into (for a stacked pull request, the head of the branch below). The
changed paths and their risk floors come from that base, so a receipt for a
delta review still covers the whole pull request. The lander proves each head
against the fetched target: the reviewed head on the reviewed base, a
merge-in from the target, or a rebase with an unchanged delta keeps the
review. An unchanged head retargeted past the branch below it does not,
because that branch's changes were never reviewed; any other change
excludes the pull request until a new receipt covers it. A receipt whose proof
cannot be read excludes only its pull request, as `unprovable`, and one saved
before this shape is reported as `receipt shape retired: save it again`. Forge rules
and required checks still apply.

Every PR gets an outcome receipt under the project state's
`landing/receipts/`: landed, red, conflicted, excluded or pending-unknown, with
each unmet check named. Batch errors distinguish `LAND_CONTENTION`,
`LAND_CONFLICT`, `LAND_GATE_UNMET`, `LAND_PUBLICATION_UNKNOWN` and
`LAND_SPENDING_CAP`.

#### Recover an interrupted landing

The lander writes a marker under the project state's `landing/pending/` before
publishing. An ambiguous outcome, such as a timeout or 5xx, keeps it, and the
error names its path. Do not remove it to force a retry. Instead:

1. Restore access to the forge and target.
2. Run `git fetch origin <target>` and
   `git merge-base --is-ancestor <candidate> origin/<target>` for the recorded
   candidate.
3. Check each PR with
   `gh pr view <number> --json state,headRefOid,mergeCommit,baseRefName`.
4. After the in-flight window (`publish_settle_seconds`), rerun
   `provenant land <number>` without `--shadow`. Recovery observes the outcome,
   finishes receipts and cleanup, and deletes the marker last.

A refused local cleanup reports `cleanup_pending` with its steps and does not
block later landings.

#### Single-PR landing

`provenant land 148 --wait` waits up to an hour for CI (`--wait=30m` changes
it, up to four hours) and merges over REST at the verified head; `--head SHA`
pins the reviewed head. A CI timeout exits 124 without merging, and a queued or
auto-merge exits 5 (`pending`). Rerun with `--wait` after `CI_PENDING`.
`CI_CONFLICT` means the PR conflicts with its base, so CI never starts: merge
`main` or rebase, then push. `CI_NOT_SCHEDULED` means no check registered for
the head. `RATE_LIMITED` reports the reset and a retry command;
`MERGE_STATE_UNKNOWN` means rerun `land` to confirm the state.

After a merge, `land` fetches the base, fast-forwards the local base, deletes
the branch's declared build output and worktree, and deletes the local branch,
but only when the local branch tip is exactly the merged head. A refused step
reports `cleanup_pending: true` in `data.steps` and the merge still exits 0;
fix the cause and rerun, and an already-merged PR retries only the cleanup.
`land 148 --full` shows every step.

### Automation identity (GitHub App)

Landing, the CI observer and status reads can authenticate as a GitHub App
installation, which gives them their own REST bucket instead of the user's
5,000 per hour. Owner setup, once per repository:

1. Create a GitHub App with repository permissions Contents and Pull requests
   read and write, and Issues, Actions and Statuses read. Install it on the
   repository. Do not add it to any ruleset's bypass or reviewer lists.
2. Store the private key outside every repository in a regular file with mode
   0600 (`chmod 0600 /path/to/key.pem`). The key is supported only as a file:
   Keychain storage is not supported because a key in any keychain stays
   readable by Keychain-granted lanes (agy), as securityd opens the keychain
   on the lane's behalf and denying the file path is not a boundary. Provenant
   denies reads of the configured key file path to every lane's sandbox,
   whatever its role, so it is not reachable from a lane.
3. Add an `app` object to the project's `github.json` in the state directory:
   `{"app": {"app_id": 123, "installation_id": 456, "private_key_path": "/absolute/path/key.pem"}}`.
   `installation_id` is optional and is looked up when absent. To serve
   every repository under one owner, key the entry by owner instead:
   `{"apps": {"OWNER": {"app_id": 123, "private_key_path": "/absolute/path/key.pem"}}}`.
   An `app` entry in a project's own `github.json` overrides the owner's.

Provenant mints a one-hour installation token, reuses it until five minutes
before expiry, and reads the key only when minting. With no App configured it
uses the user's `gh` login and warns once per process. The App identity only
reads and merges, and only on exact endpoints (REST reads, the pull-request
merge call, inline GraphQL queries); review verdicts, approvals, comments and
every GraphQL mutation stay on the user's identity. Each token is limited to the
one target repository. A lane never mints a token, and every lane's sandbox
denies reads of the key file. Lanes keep the user's login. `lanes.github_request_budget`
(default 100, 0 off) stops a lane that makes more `gh` requests than that, and
the lane's settlement records its `github_requests`.

The request budget is a pacing aid, not a security boundary. It counts `gh`
invocations, so one `gh api --paginate` call makes many requests but counts
once, and the counter file lives in the lane's scratch, so the lane can write it.

### Merge author identity

REST merges use the authenticated GitHub account's commit email; local
`user.email` does not apply. Configure that email in GitHub before an
agent-initiated land, then check the merge commit's author:

```sh
pr=<number>
merge_sha="$(gh api "repos/{owner}/{repo}/pulls/$pr" --jq '.merge_commit_sha')"
gh api "repos/{owner}/{repo}/commits/$merge_sha" --jq '.commit.author.email'
```

An unexpected author is a publication failure: stop before claiming the merge
is ready and record the evidence.

### Dependabot auto-merge

A Dependabot patch or minor bump that changes only manifests and lockfiles
auto-merges once `ci-status` is green, through
[`dependabot-automerge.yml`](../../.github/workflows/dependabot-automerge.yml).
A major bump, a new dependency or any other change takes at least one
`routine` reviewer.

### After merge

1. For a software delivery, sync the primary checkout. Unless the receipt
   already stood as the landing approval, record the accepted receipt with
   `provenant workflow receipt save` after the merge commit's `ci-status` on
   `main` succeeds. Never fabricate user approval.
2. Confirm the issue closed, or close it with its terminal reason, and confirm
   Status is `Done`.
3. Prune the merged branch's artefacts as the last step of the merge.
   [Post-merge pruning](../worktrees.md#ownership-and-cleanup) owns the authority,
   proof gates and commands. `delete_branch_on_merge=true` usually removes the
   remote head already; `git remote prune origin` clears the stale tracking ref.
   Forced deletion, and deleting an unmerged branch on either side, is a user
   gate.

CI behaviour, local reproduction of CI failures and advisory test signals are
in the [CI runbook](ci.md). The [PR reconciler](pr-reconciler.md) and
[event intake](event-intake-configuration.md) workers observe pull request
and CI events outside a session.
