# Lane safety

Failure modes that cost lost work, a damaged primary checkout or a confident
false report. Paste the first section into every lane and reviewer brief.

## Protect the primary checkout

- Briefs forbid the [git-verb denylist](chair-loop.md#reviewer-git-verb-denylist)
  and any test suite, build, browser walk or process that starts a database or
  server in a shared checkout. Before trusting a review, check no suite ran;
  cancel and redispatch a lane that started one.
- Direct process control is refused. Use `provenant cancel TARGET` for owned
  work. The optional Bash hook is `provenant hook claude pre-tool-use`;
  confinement and owned process handles enforce the underlying policy.
- `provenant hook git pre-push` refuses default-branch deletion,
  non-fast-forward pushes, default-branch pushes from linked worktrees and
  conflict markers in pushed commits. `install --hooks on` registers only
  the fixture-backed hooks in the Rust registry. The primary checkout keeps
  its legacy copies until the authorised cut-over.
- A lane has no inbox: it hears new instructions only through `resume` or a new
  dispatch.

## Preserve lane work

- Writers often finish with the deliverable uncommitted. Commit it first
  (`wip(<lane>): deliverable, pending review`), amend after review. A
  mutate-and-revert pass using `git checkout -- <file>` silently wipes
  uncommitted work.
- Long-running writers push and open a draft PR early and push after every
  meaningful step, so a cut-off loses nothing.
- Recovery: the lane transcript usually holds the final `git diff`; apply it
  with `git apply --check` first and compare `git diff --stat` with the report.

## A lane's report is a claim

- Read `git status` and the full diff; discard what the brief did not ask for;
  never `git add -A` on the strength of one file.
- A mid-flight `SendMessage` correction to a running lane can be refused as
  prompt injection while a sibling accepts it. Treat an unacknowledged
  redirect as not delivered: stop and redispatch with the corrected brief, or
  verify uptake from the commits, report file or transcript.
- A worktree without installed dependencies runs nothing, yet lanes report
  passes. Install and prove it with one test file before dispatch.
- Read lane state from `fabric_status{detail:"full"}`, not ad-hoc `ps`. Record
  the tip of each writer lane (one expected to deliver commits) on first sight:
  an unchanged tip with nothing ahead is `empty`, a failure even when the run
  says `ok`. Read-only reviewers and researchers commit nothing; judge them by
  their artifacts and evidence.
- Before accepting a green, read the last exit code of every verify command in
  the lane's event log; any nonzero last exit voids it.
- Targeted tests chosen from the changed test files miss dependents. Also run
  the tests that import the changed modules, and the full suite for a copy or
  contract change.
- A rebase that adds a keyed entry (object key, registry or inventory line) can
  duplicate one the base already has; grep the merged tree for each added key.
- A failed lookup must be loud: one failed `gh pr list` made every branch read
  as "no PR".
- Merge briefs are snapshots; members added after launch are missed. Resend the
  full list.
- `git commit --amend` after a formatter run commits nothing new unless the
  files are staged. Stage and confirm `git status --porcelain` is empty.

## Fabric and harness

- Reconnecting the Fabric MCP server cancels every lane it hosts. Commit
  writers' work or wait for idle first.
- `fabric_cancel` with a batch task id cancels the whole batch; dispatch writers
  as single-task runs. A cancel can reap a process another lane depends on.
- A lane must not start a nested Fabric dispatch; sandboxed Codex lanes reject
  it before placement because host state is outside their writable roots, so
  dispatch reviewers from the chair.
- Headless Claude lanes exit when the turn ends: brief them to run long jobs in
  the foreground, take baselines in a separate detached worktree and end only
  with the final report.
- Renaming a PR's head branch closes the PR. Name the branch before the first
  push; to fix a name, open a new PR that supersedes the old one.
- Edits to protected instruction files during a run are quarantined or kill the
  run; see [routing-and-tiers.md](routing-and-tiers.md). The
  coordinator commits instruction-file edits first.
- A permission classifier may refuse edits that loosen a security test or its
  exemption list. Park that lane, record the exact edit for the owner and never
  route it to another agent.
