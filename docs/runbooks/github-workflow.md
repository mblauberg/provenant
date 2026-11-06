# GitHub work-item workflow
Owner: repository maintainers
Anchor: .github/ISSUE_TEMPLATE/work-item.yml; [`landing::driver::run`](../../tools/front-door/src/pr/landing/driver.rs)

Use this process for user- and agent-originated repository work. Project
Status is the sole workflow state; labels describe content or gates, not
progress. The label taxonomy is declarative in
[`.github/labels.yml`](../../.github/labels.yml), synced with pruning: a label
absent from that file is deleted on the next sync, so never hand-create one.
Issue intake uses the issue forms under `.github/ISSUE_TEMPLATE/`; blank
issues are disabled.

Start approved delivery with
`provenant run --use deliver --file ISSUE.md --explain`, inspect its authority and gates, then submit without `--explain`.
Use `batch` for parallel slices of about three acceptance items merged into one
integration branch. Review each slice lightly and the integrated head in full;
shared contracts precede dependent slices. See [templates](presets.md).
Observe with `status`, collect `output TARGET` and `output TARGET --trace`,
use `steer` for a message, `stop` for a subtree, `retry` for unfinished work
and `edit` for unstarted graph nodes. `templates`, `land`, `quota` and `trace`
complete the core interface; `help --all` lists advanced commands.

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

## Change spec

The issue body owns the ephemeral change spec; its authority ends with the
delivering PR, and any rule it settles moves to its durable home in the same PR.
Write it with the [`scope`](../../skills/scope/SKILL.md) skill: the owner approves
one digest, and implementers cannot change the acceptance criteria.
`scripts/check-scratch-paths` warns
about possible scratch citations in tracked Markdown and issue templates; its
findings are advisory. Spec drift is a reviewer check at acceptance, never an
automatic gate.

## Agent filing

Filing follows the [`tracker`](../../skills/tracker/SKILL.md) skill: recorded
duplicate searches, one issue per outcome, `source:agent` and the `Filed-by:`
trailer, and evidence for every agent-filed defect. Issues are Epic, Feature,
Task or Bug; there is no Decision issue. Only architecture becomes an ADR, other
settled calls go to the doc they change, and owner-only questions go through
`provenant ask`. This repository's Ready cap is advisory, set at about three times the
concurrent writer lanes, which follow provider quota and the chair's judgement.
Pause new filing at the cap and deliver or reconcile existing work.

Re-check issue content when a referenced PR merges, a dependency closes or
evidence moves. Age, silence and `updatedAt` cannot establish staleness or
completion; bulk edits refresh timestamps without resolving the issue.

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
issues own independently deliverable slices; the tracker skill covers when a
leaf is created. Create or link a child with `gh issue create --parent N` (and
`--blocked-by` for ordering) when the installed `gh`
supports it, otherwise through GitHub's `sub_issues` API, never a checklist or title.

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
   head needs CI; apply the review ladder head-move rules, with A and B in
   shadow until enforced and current exact-head landing gates retained.
   At acceptance, the reviewer checks spec drift and records any mismatch for
   resolution; this judgement is advisory, not an automatic gate.
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

Name the branch `<type>/<area>-<slug>`, at most 48 characters, where `<type>` is
`feat`, `fix`, `refactor`, `perf`, `test`, `docs`, `chore`, `ci` or `build`, for
example `fix/repo-runbook-mechanics`. Use no issue number (one branch may cover
several issues or a slice) and no run vocabulary such as batch, wave, lane or
model name. The worktree directory is the branch with `/` replaced by `-`. The
standing `HARNESS.md` envelope covers branch and worktree creation without
extra authority. An authorised merge covers pruning its own worktree and
merged refs; the chair decides other removals, as `GLOBAL-AGENTS.md` sets out. Dirty worktrees are refused.

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
too once merged. `--discard` deletes an unmerged branch; the chair uses it
for superseded or abandoned work and records the returned head.

Then set the issue to `In progress` (commands under
[Project status](#project-status)).

### Commit and push

Reference each covered issue from a commit body with `Refs #N`. Never put a
closing keyword in a commit message; the pull request owns issue closure. Push
with an upstream so `gh pr create` finds the branch:

```sh
git push -u origin fix/repo-runbook-mechanics
```

A Fabric writer has no Git credentials unless `lanes.publish` is on (see
[lane lifecycle](lane-lifecycle.md)). Otherwise use
`provenant run --use deliver --file BRIEF --explain` and check that the composite
binds the intended branch and host delivery before submitting. The host pushes
the verified commit and opens the pull request within the approved authority.

### Pull request

Open the pull request against `main` (or its stack base as below) and link the
issue per the rule in
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

### Stacked pull requests

Use a stack for dependent slices of one outcome that merit separate review: a
shared seam and its consumers, a second batch built on the first, or a large
refactor in reviewable steps. Independent changes get separate PRs off `main`;
do not stack a slice that cannot pass CI alone.

GitHub stacked PRs are generally available, with no repository enablement.
They form a linear chain in one repository, without forks: the bottom PR targets
`main` and each later PR targets the branch below. The merge box shows the
stack map. Each PR carries its own `Closes #N` or `Refs #N` per issue, following
the closure rule above; review its own diff against the branch below.

With a current `gh`, install `gh extension install github/gh-stack`. Run
`gh stack init`, commit the first slice, then `gh stack add BRANCH` for each
new branch on top. `gh stack submit` pushes branches, creates or updates PRs
and creates the stack. `gh stack view` shows it; `gh stack unstack` dissolves
it. A completed stack cannot be extended: a new submit starts a new stack.
Alternatively create and extend stacks through the API. REST PRs carry a
`stack` object; `GET /repos/{owner}/{repo}/stacks` lists stacks, and REST has
create, extend and dissolve endpoints. GraphQL stack fields are read-only;
webhooks carry `stack`.

The chair normally merges `main` into branches instead of rebasing. A writer
may rebase and force-push with lease its own unmerged branches, including a
whole stack, never `main` or another writer's branch. Provenant's pre-push
handler refuses non-fast-forward updates only to the default branch. `gh stack rebase`
cascades and supports `--upstack` and `--downstack`; `gh stack push` uses an
atomic per-branch force-with-lease, and `gh stack sync` fetches, rebases, pushes
and syncs PR state. Recheck the changed heads and their review evidence.

Required checks and branch protection apply to every PR, based on the bottom
PR's base; stack merges cannot bypass them. CI runs for every PR; use
`github.event.pull_request.stack` to limit expensive jobs when appropriate.
Land bottom-first with `provenant land`. Stack members use the asynchronous
REST merge API, with the configured merge method and each verified head pinned.
An upper PR waits for its lower members; after retargeting, its new head needs
its own CI and review evidence. A pending or ambiguous merge is reconciled
before retry. Prune merged branches and worktrees as usual after landing.

See GitHub's [overview](https://docs.github.com/en/pull-requests/get-started/about-stacked-prs),
[quickstart](https://docs.github.com/en/pull-requests/get-started/stacked-prs-quickstart),
[CLI reference](https://docs.github.com/en/pull-requests/reference/stacked-prs-cli-commands)
and [rollout guide](https://docs.github.com/en/pull-requests/tutorials/roll-out-stacked-prs).

### Project status

Project Status (project `2`, owner `mblauberg`) is the sole workflow state; no
effort or session document owns it. Ownership of each transition:

| Transition | Owner | When |
|---|---|---|
| `Backlog` to `Ready`, `Done` or unchanged | Triage: user, or an agent inside granted authority | The triage result is recorded in an issue comment |
| `Ready` to `In progress` | Implementing agent | Work on the accepted scope starts |
| `In progress` to `In review` | Implementing agent | The pull request, exact-head checks or independent review is active |
| `In review` to `Awaiting user` | Implementing agent | Machine gates pass; a required user decision remains |
| Any later state to `Done` | Project workflow, when the issue closes or its pull request merges | No gate remains; the terminal reason or integrated pull request is recorded |

The project's workflows also set Status: linking a pull request to an issue, merging it and closing an issue each move the item, and setting `Done` closes an open issue, so never set `Done` on work that should stay open.

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
same-family substitution, through a different lens without reducing the count,
when cross-family review is unavailable. This
repository is a personal harness, not production: by user directive, repository auto-merge is enabled
and agents merge directly. An agent merges a pull request once it has passed
its tier's planned independent review coverage and `ci-status` is green on the
exact PR head for single-PR landing or on the candidate for batch landing,
without waiting for the user. Forge-required checks also remain mandatory. `gh pr
merge --auto` may be queued once those gates are met.

`ci-status` is the single required check on branch protection. It is the
aggregate job at the end of [`ci.yml`](../../.github/workflows/ci.yml): it
runs on `if: always()`, succeeds only when every job in its live `needs:` list
either succeeded or was skipped by the path filter, and fails closed on any
failure or cancellation. "CI is green" means exactly this one context; no
other check is required.

Use `provenant land 148 149` to land an ordered batch. The candidate gate
checks `ci-status` on the candidate for this repository; the ruleset requires
pull requests, so `auto` selects `rest_ordered` publication. The forge still
enforces its required checks. See [batch landing](#batch-landing) for policy,
evidence and recovery, and [single-PR landing](#single-pr-landing) for the
existing single-PR path.

### Open a pull request from receipts

```sh
provenant pr open --from-attempt <writer-attempt> --review <review-attempt> \
  --summary-file summary.md --issue 1829 --dry-run
```

`pr open` builds the body from the repository's pull request template and the
attempts' own records, so the routes, receipt paths and heads in it are the
recorded ones. The summary file's first line (a heading or plain text) is the
title and the rest is the Summary section. Check the dry run, then drop
`--dry-run` to run `gh pr create` for the attempt's delivered branch, which
must be at the attempt's head on the remote. Complete the gate rows marked
`pending` with the exact command and result before asking for review. The
attempt must be local and `succeeded` or `review_required`, with a recorded
head; read-only reviews supply the reviewer rows. `provenant pr status 148 149`
prints `#148 OPEN <head> checks=3/4 pending=1 merge=blocked review=none`, one
line each, from one head-and-checks snapshot per PR. Use
`provenant pr status 148 --wait=5m` to block until that snapshot changes;
an unchanged snapshot at the limit exits 124. If checks cannot be read,
the line says `checks=unavailable` and the JSON check counts are null. An empty
checks result is also unavailable. A failed PR lookup appears as an unavailable
row and preserves other PRs' results. Human output is bounded to 2 KiB;
`--full` includes every row and `--json` carries the complete machine envelope.

### Batch landing

```sh
provenant land 148 149
provenant land 148 --batch
provenant land 148 149 --shadow
provenant land --tier substantial --explain --json
```

Multiple numbers select the batch path; with `integration = "batch"`, a plain
single number also runs as a batch of one, with the exact-candidate gate and
`auto` publication. `--batch` explicitly selects the batch path for one PR.
`--wait`, `--head` or `integration = "single"` select the single-PR path. `--shadow`
builds and gates the candidate and records the decisions without publishing;
only the candidate ref may change, and it is removed after the decision is recorded.
`--tier` selects a risk-tier override: `routine`, `substantial`, `crucial` or
`terminal`.
`--explain --json` needs no numbers and reports effective policy values and
their sources. `--head` and `--wait` apply to single-PR landing only.

The lander selects eligible PRs with pinned heads, independent approvals and the
configured target, then chains one merge commit per PR onto the fetched target.
The approved head is each merge commit's second parent. A gate on the final
candidate binds its commit, tree, definition, issuer and coverage; missing,
skipped, neutral or mismatched required evidence does not pass. A red batch
splits into ordered prefixes; a conflict isolates the PR after the clean prefix.
The lander rechecks heads, base branches and approvals; retargeted PRs are
excluded. It rebuilds and re-gates if the target moves, up to the configured
limit. No PR branch needs updating.

`fast_forward` advances the target from the expected base without force.
`rest_ordered` checks the target before each merge against the expected base or
previous merge commit, then merges pinned heads through the forge in order. A
mismatch stops publication and rebuilds the remainder within `max_rebuilds`.
A target update can still race between that check and the forge merge; the final
tree comparison alarms if the result differs from the gated tree. `auto` selects
the publication mode based on direct-push authority and records why.

A stale or non-fast-forward push, merge response 405, 409 or 422, or head
mismatch is a definite refusal. The lander reconciles and receipts anything
already landed; target movement consumes a rebuild, while another refusal is
reported after follow-up completes. The lander writes a publishing marker before
publication, so an interrupted run also leaves one. A lost or ambiguous response,
such as a transport error, timeout or 5xx, keeps it. Recovery checks ancestry and
forge state before any retry. Once the outcome is definite, the marker becomes
settled: it records each outcome and the remaining receipt, follow, tree comparison
and candidate-ref cleanup steps. A settled marker never authorises publication.
Recovery completes those steps and deletes the marker last. A refused local
cleanup reports `cleanup_pending` and its steps, leaves the marker for an
opportunistic retry, and does not block later landings. A transport or I/O
failure during local cleanup also leaves the marker for retry without blocking
recovery of later pull requests.
The error gives the marker path: inspect its JSON under the project state
`landing/pending/`, and compare its candidate and PR heads with the target's
ancestry and forge PR state. Do not remove it just to force a retry. If the
marker stays unknown, restore access to the forge and target. Fetch the target
with `git fetch origin <target>`, then check the recorded candidate with
`git merge-base --is-ancestor <candidate> origin/<target>` and the PRs with
`gh pr view <number> --json state,headRefOid,mergeCommit,baseRefName`.
After the in-flight window, rerun `provenant land <number>` without `--shadow`;
recovery observes the outcome and completes the recorded follow-up.
If the forge state or ancestry remains unavailable, restore that access before
retrying. The tree comparison uses the recorded landed commit, even if the target
has since moved. Shadow mode does not write a marker or perform recovery.

Every PR receives an outcome receipt, including landed, red, conflicted,
excluded or pending-unknown outcomes. Shadow receipts include the publish mode
and reason and split decisions; no publication occurs. Gate failures name each
unmet check and why it was unmet: missing, failed, skipped or neutral, wrong
issuer, or another tree. Receipts also retain base, candidate, gate, policy
sources, cost and alarms and map approved SHAs to landed SHAs. Post-merge
checks can bisect failures and raise an alarm. `revert_forward` requires
`post_merge.bisect` to identify a culprit; the lander warns when it is disabled.
When `revert_forward` is enabled,
the adapter creates a mainline-parent revert on the current target, pushes the
new `<candidate_ref_prefix>revert/<pr>` branch only if absent, and opens a PR
naming the culprit and failed checks. Any failure raises an alarm. Batch error
envelopes retain outcomes recorded so far and distinguish `LAND_CONTENTION`,
`LAND_CONFLICT`, `LAND_GATE_UNMET`, `LAND_PUBLICATION_UNKNOWN` and
`LAND_SPENDING_CAP`.

#### Landing settings

Set these under `[domains.pr.land]` in host `config.toml`. The generated
[configuration reference](../reference/config-keys-all.md#host-land-settings) lists
defaults, values and bounds for every key. Policy resolves bundled defaults,
host settings, `projects.<project>`, `tiers.<tier>`, then invocation overrides;
omitted fields inherit. The project and tier tables accept the same fields.

- `poll_seconds` controls initial CI polling; `--poll` overrides it.
- `integration` selects `batch`, `single` or `forge_queue`; `batch_size` and
  `batch_wait_seconds` bound collection. `candidate_ref_prefix` names temporary
  refs and `max_rebuilds` bounds rebuilds on target movement.
  `publish_settle_seconds` defaults to 30 and controls the wait before recovery
  re-observes a publication that cannot safely be retried. Setting it to `0` disables
  the in-flight window and `land --explain` warns. `publish_unknown_warn_seconds`
  controls when an unresolved marker emits a warning with the recovery command
  (default 3600 seconds); it does not resolve the outcome. The bundled CI
  trigger covers `land/**`; if you change the prefix, update the CI trigger to
  match. `land --explain` warns on a non-default prefix. The contract's forge
  queue path is not yet implemented and `forge_queue` is rejected at runtime.
- `gate.kind` selects checks or commands. `gate.checks` and `gate.issuer` name
  required checks and optional issuer; `gate.commands` and `gate.executor`
  specify a command plan and its executor. `gate.timeout_seconds` and
  `gate.coverage` bind its limit and declared coverage. Command gates remove
  `GH_TOKEN`, `GITHUB_TOKEN` and `GH_ENTERPRISE_TOKEN` from the environment
  unless named in `gate.env_passthrough`; the command process group is stopped
  on every exit. Git credential helpers and keychains remain available to the
  command under the user's normal environment.
- `pr_ci.mode` selects `always`, `opt_in`, `affected` or `none`; `pr_ci.label`
  names the opt-in label. This policy cannot bypass forge-required checks.
- `publish` selects `auto`, `rest_ordered` or `fast_forward`; `method` selects
  `merge`, `squash` or `rebase` for `rest_ordered` only.
- `post_merge.checks`, `post_merge.bisect` and `post_merge.revert_forward`
  configure the follow-up plan, failure isolation and revert authority.
  `post_merge.timeout_seconds` defaults to 3600; follow polls until all checks
  settle or the timeout raises an `unsettled` alarm.
- `budget.rate_micros_per_minute.<pool>` prices a pool; `budget.advisory_micros`
  warns and `budget.cap_micros` sets a spending boundary, in millionths of the
  configured currency. `gate.pool` and `post_merge.pool` select the pools for
  gate and post-merge duration; a checks gate falls back to the check issuer
  app slug when no gate pool is configured. Unknown cost never refuses; the
  cap refuses only priced spend.
- `local_gate.mode` is `warn`, `enforce` or `off`; `local_gate.commands` and
  `local_gate.timeout_seconds` configure the fast pre-push plan. With no commands
  nothing runs. A failure warns or blocks according to the mode.

For a migration from a bespoke lander, see [landing migration](landing-migration.md).

### Single-PR landing

The single-PR path, selected with `--wait`, `--head` or `integration = "single"`,
uses the following behaviour. `provenant land 148 --wait` waits up to one hour for CI, merges at the verified
PR head, then runs the local follow-up below. `--wait=30m` changes the limit,
up to four hours; omit `--wait` for one check. `--head <reviewed-sha>` pins
the reviewed head. Waiting reads check runs and legacy statuses by the pinned commit SHA,
verifies required check names have registered and checks the head before and
after each sample. Failed checks or head movement
stop the wait. Progress uses at most eight stderr lines; JSON stdout remains
one machine envelope. CI timeout exits 124 without attempting a merge.
A merge that is queued or left to auto-merge exits 5 (`pending`); if a merge
call errors but the PR is merged, the report is landed. With no required
check, landing also waits 60 seconds after the newest check registers. If
`land` reports `CI_PENDING`, rerun it with `--wait`.

Landing uses REST for pull request resolution, checks and merge requests, which
are pinned to the verified SHA. Pending checks are sampled initially after 30
seconds, then with backoff up to four times that interval. Set the host default
with `domains.pr.land.poll_seconds` in the state-root `config.toml`, or pass
`--poll SECONDS` for one invocation. `land --explain --json` reports the
effective landing policy and its sources; it does not report the single-PR retry
budget. That budget defaults to 60 seconds without `--wait` and otherwise uses
the requested wait duration. The `--wait` duration also bounds retries of
GitHub rate limits; `RATE_LIMITED` reports the reset and a retry command.
Without server timing, secondary limits wait at least a minute and back off,
following [GitHub's rate-limit guidance](https://docs.github.com/en/rest/using-the-rest-api/rate-limits-for-the-rest-api#exceeding-the-rate-limit).
`MERGE_NOT_ALLOWED` means GitHub refused the merge; resolve the reported
mergeability problem and retry. `MERGE_STATE_UNKNOWN` means a merge was
attempted but its result could not be confirmed; rerun land to confirm the
state and finish any cleanup.

Nothing after the merge changes what merged, and no step forces. A refusal is
reported in `data.steps` with `cleanup_pending: true` and reason
`cleanup_pending`. The merge still exits 0. Inspect `land 148 --full`, fix the
reported cause and rerun: an already-merged PR skips CI and merge and retries
only safe cleanup. CI failures use `CI_PENDING`, `CI_FAILED`,
`CI_UNAVAILABLE`, `CI_POLICY_UNAVAILABLE`, `CI_TIMEOUT`, `CI_CONFLICT`,
`CI_NOT_SCHEDULED` or `HEAD_MOVED`. `CI_CONFLICT` means the PR conflicts with its
base, so CI never starts: merge `main` or rebase, then push. `CI_NOT_SCHEDULED` means the wait
ended with no check registered for the head. Required checks on the exact head
stay mandatory.

| Step | Does | Refuses when |
|---|---|---|
| `fetch` | `git fetch <remote> <base>` | the fetch fails, or the fetched base does not hold the merged head |
| `fast_forward` | moves the local base to the fetched one, in the checkout that has it, or by fetching `base:base` when none does | that checkout has any change, or the base has diverged |
| `worktree` | deletes declared build output (`gc.projects.<id>.regenerable`, plus `target`), then removes the branch's registered worktree as `worktree remove` does | the worktree is dirty or has a live writer, this command runs inside it, its HEAD is not the merged head (it stays whole), or its HEAD moves after the check, which removal re-verifies under the worktree lock |
| `branch` | deletes the merged local branch with `git update-ref -d <branch> <merged head>`, which deletes only if the branch is still at the merged head (a branch removed with its worktree is already gone) | the branch holds commits the fetched base lacks, any worktree has it checked out, mid-rebase or mid-bisect, or it moved off the merged head after the check (it is kept); a checkout, rebase or bisect starting between the check and the delete is not caught |

The `worktree` and `branch` steps act only on a local branch whose tip is the
exact head the pull request merged. When the pull request's head is in another
repository, such as a fork, or the local branch of that name is at any other
tip (recreated at the base, or advanced past the merged head, even when the
base contains it), both steps are skipped with that reason and nothing is
touched. A tip the fetched base lacks is refused rather than skipped.

The build-output deletion keeps GC's checks: tracked and `keep` paths stay,
symlinks are never followed, and a lane that is not idle blocks it. Unlike a
sweep it does not wait for output to be a day old, because the merge ends the
work. A merge that is only queued reports `merged: false` and leaves everything
in place.

The helpers use the current repository, or the global `--project` selection.
`--json` includes `number`, `head` and `result` in the normal command envelope.
After a successful merge, JSON also includes `merged: true` and text says `merged`.
Landing merges over REST with `sha=<verified head>` and `merge_method=merge`.
GitHub uses the authenticated account's commit email; the local Git
`user.email` does not override it. Verify the resulting author as below.

### Merge author identity

For an agent-initiated land, configure the authenticated GitHub account's
commit email to the intended address in GitHub before merging. REST does not
accept `--author-email`. After landing, inspect the generated merge commit's
recorded author:

```sh
pr=<number>
merge_sha="$(gh api "repos/{owner}/{repo}/pulls/$pr" --jq '.merge_commit_sha')"
gh api "repos/{owner}/{repo}/commits/$merge_sha" --jq '.commit.author.email'
```

For a queued merge, inspect the author after GitHub has created the merge
commit. An unexpected author is a publication check failure: stop before
claiming the merge is ready and record the generated author evidence.

For status-check rollup handling and false-green traps, follow the
[shell-pitfalls runbook](shell-pitfalls.md#shell-pitfalls).

The user review/merge gate applies only when the agent is stuck: split review
verdicts it cannot settle with primary-source evidence, or a decision outside
its granted authority. An exhausted repair budget triggers the ladder's
principal scoping, not an owner wait within approved scope. Standing user gates are:
credential or connector setup, rewrites of or pushes to shared branches (including
`main`) outside authorised merges, and risk-tier downgrades. Deleting a *merged* branch's own
local ref and worktree is carried by the merge itself, under
[post-merge pruning](../worktrees.md#ownership-and-cleanup); removing other
branches and worktrees, unmerged ones included, is the chair's judgement.

The live `Protect main` ruleset sets `strict_required_status_checks_policy: false`,
so branch protection does not require the head to be up to date with `main`.
For batch landing, `ci-status` gates the candidate; changes that conflict are
reported through the ordered split. For overlapping single-PR landings, use a
serial merge train: merge one, update the next onto the new `main`, rerun
exact-head checks and apply the review ladder head-move rules before merging.
The ladder keeps rules A and B in shadow until enforced.
Landing reports a head-move classification as shadow evidence; it does not
replace review on the exact head. Find it in the report warnings and the
`head_moves` field of the JSON report, candidate receipt and run summary under
the project state directory’s `landing/receipts/`. An approval-time base not
recorded when the review was made remains unknown, and incomplete delta
statistics classify as C. For class B, line changes accumulate over each
single-parent commit. Bundled review path exclusions and instruction controls
require class C; add project-specific exclusions with
`git config --add provenant.reviewProtected 'pattern'`. Patterns match paths
relative to the repository root; `*` matches within a component and `**`
across components. These extra patterns do not remove the bundled exclusions.

### Rust build cache

Each Rust cache step caches `$GITHUB_WORKSPACE/tools/front-door/target`,
including Cargo's nested build-dir layout. A `build.build-dir` outside that tree is not cached
unless its path is added to the action's `cache-directories`.

### Advisory test signals

`.github/workflows/test-signals.yml` reports test strength on pull requests.
It never joins `ci-status` and its scripts exit 0, so everything warns and
nothing blocks a merge. Read the warning annotations and job summaries; the
signals run in CI only.

- **Assertion diff:** flags `assert_eq!` replaced by `assert!`, a specific
  variant check replaced by `is_err()`, and removed assertions, tests or
  `#[should_panic]` in test files. A line heuristic, so expect some noise.
- **Stress run:** runs only the tests the pull request added or changed, with
  nextest `--stress-count 20` and no retries, to expose flakiness.
- **Mutants:** `cargo-mutants --in-diff` runs nightly, and on a pull request
  carrying the `mutants` label. It skips files that mention `unsafe` and runs at
  most 40 mutants. Surviving mutants are warnings.

`--in-diff` mutates only changed source lines, so a diff that touches only
tests runs no mutants and gets no mutation signal.

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

### Dependabot auto-merge

Dependabot updates are exempt from review: a patch or minor version bump that
changes only manifests and lockfiles may auto-merge once `ci-status` is green.
A major bump, a new dependency, or any change outside manifests and lockfiles
takes at least one `routine` reviewer.
[`dependabot-automerge.yml`](../../.github/workflows/dependabot-automerge.yml)
queues `gh pr merge --auto` for eligible updates after checking the metadata
and changed paths. Other updates wait for review.

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
   automatically deletes a merged pull request's remote
   head branch, so the remote ref is usually already gone by the time you prune;
   `git remote prune origin` clears the stale remote-tracking ref. This
   automatic merged-head cleanup needs no separate per-branch authority. Forced
   deletion, and deletion of an unmerged branch on either side, remains an
   explicit user gate.

### Event intake

The standalone worker is not yet included in the release installer. Build it
from the checkout with `cargo build --release --locked --bin event-intake` in
`tools/front-door`, and run the resulting binary from its Cargo target directory.
Run one `event-intake` worker for each repository and state directory. Its
[configuration reference](event-intake-configuration.md) lists the environment
variables. With `PROVENANT_INTAKE_BIND` unset it runs polling only. To receive
webhooks, bind to loopback, put an operator-managed stable HTTPS tunnel in front
of `/webhook`, and configure a repository webhook with the same secret and
these events: pull request, push, check run, check suite, status, issue comment,
pull request review and pull request review comment. The tunnel provider is an
operator choice; keep the endpoint private to the tunnel where possible.

The worker runs an authoritative sweep at startup and every 300 seconds by
default. It also reconciles signed hints, coalescing duplicate targets and
turning unmappable checks into repository sweeps. Requests use `gh` credentials
and persisted ETags. Keep the state directory private: it contains response
bodies, delivery IDs and timing data. Do not run two workers with the same
state directory. Existing `gh` commands do not depend on the worker. Run
`event-intake status` with the same repository and directory to inspect counters,
304/200 ratio, rate-limit headers, sweep watermark and oldest pending marker.

Before relying on the service, the chair must record two live checks on a test
repository and configured tunnel. For latency, send at least 20 each of push,
completed check, comment, review and merge events; record the event time and
when the corresponding current state appears in `state.json`, then calculate
p95 per event type and record request counts and 304/200 ratio from status.
The `deliveries.json` markers include the received and completed times and event
name, which help attribute samples; compare with the independently recorded
event time to include delivery delay. Each p95 must be below 30 seconds. For
the outage drill, stop the receiver or tunnel, make changes including a merge,
restore it and verify against GitHub that the first successful sweep captured
all changes. Record times, PRs, status counters and results. Do not claim these
live checks from fake-forge tests.
