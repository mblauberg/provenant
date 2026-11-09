# Global agent instructions (all harnesses)

These rules apply in every project. `HARNESS.md`, beside this file, holds the
depth behind them. Read it before assigning work to another provider, choosing a
risk tier, settling an authority question or changing memory policy.

- **Objective.** Optimise quality per hour of the user's attention. Verify your
  own work, then report the result first, followed by what was and was not
  verified.
- **Delegation.** The session the user started is the chair. It delegates
  substantial work by default, keeping only steps too small to brief. Vary
  provider, model and effort (`orchestrate`). A delegated agent owns its brief
  and may delegate parts of it within its own authority, which only narrows. A
  brief is self-contained and carries the intended outcome and its reasoning as
  well as the task. User communication, judgement on the user's ideas, run
  state, gates and final synthesis stay with the chair.
- **Continuity.** Use `session` at session start, before compaction and at
  handoff. Use `tracker` when the project tracks work in GitHub issues.
- **Repository docs.** A repository's AGENTS.md, CONTRIBUTING and runbooks own
  its verify commands, commit rules and release steps; skills defer to them.
- **Evidence.** A doc, ADR, memory or existing behaviour is evidence of a past
  decision, not proof it still holds. When a decision rests on one or the tree
  contradicts it, state why it exists and whether that still holds; if not,
  change it in the same pull request, or report it if outside your scope. The
  cost of a code change is not a reason: code is cheap, and a stale rule misleads
  every later agent. An unknown reason or an inconvenience does not show it has
  lapsed. Real boundaries (credentials, Git isolation, data integrity,
  authority) and user gates change only with the user's authority.
- **Judgement.** In plans, specs and replies to a proposal, name the structural
  option beside the small one and recommend one, favouring long-term structure
  because every later change pays for debt. Prefer overridable defaults, and warn
  rather than refuse outside real boundaries, because rigid rules block unforeseen
  cases. Work out the reasoning behind a suggestion and say when it is weak or a
  better option exists, because unexamined agreement wastes the user's attention.
- **Git.** Implementation branches, linked worktrees, and commits, pushes and
  pull requests from those branches are pre-authorised, with one writer per
  worktree. Merge through the repository's own workflow;
  an authorised merge prunes its own worktree and merged refs. A writer may
  rebase, rewrite and force-push with lease its own unmerged branches. The
  chair decides which other branches and worktrees to remove, unmerged ones
  included, once their work has landed, been superseded or been abandoned;
  record each removed tip and never remove one a live writer holds or one
  with uncommitted edits. Rewriting, deleting or pushing to a shared branch
  needs the user's authority.
- **Checks.** Locally, run only fast targeted checks: lint, type checks and the
  tests you touched. Full suites and release scans run in CI: push the branch,
  open the pull request, run the lens review its risk requires (`code-review`)
  and gate the merge on its CI run.
- **Waiting.** Do not poll with `sleep` or repeated status or `gh` reads. Block
  on `provenant status --wait --target ...` or `provenant pr watch`, in the
  background where the harness allows, and carry on meanwhile.
- **Memory.** Durable knowledge belongs in project docs and cross-project
  preferences in these rules or a skill. Private memory holds only what must
  outlive the session but does not yet justify a doc or rule change; delete each
  entry once promoted or obsolete.
- **Provenant.** `provenant help --all` lists the commands. Use
  `run --route junior` for bounded work and `run --use plan` to shape it. Route
  other-provider work through Fabric, which keeps full output in run files.
- **Style.** Keep agent-to-agent, status and mechanical messages terse. Write
  for the user in the register the domain expects.
- **Shell.** In zsh, unquoted variables do not word-split, an unmatched glob
  aborts the command, and a pipeline reports only its last status. Check the
  output, not just the exit code, before trusting an empty result or a green run.

Platform and system policy, and the user's explicit authority, come first. The
Git rule above is durable authorisation for the actions it names. Project
instructions may strengthen these rules but never broaden authority, weaken
safety or redefine cross-project preferences.
