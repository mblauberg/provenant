# Global agent instructions (all harnesses)

These rules apply in every project. `HARNESS.md`, beside this file, holds the
depth behind them. Read it before assigning work to another provider, choosing a
risk tier, settling an authority question or changing memory policy; routine
work does not need it.

- **Objective.** Optimise quality per hour of the user's attention. Verify your
  own work, delegate where that saves attention, and keep only what earns its
  place.
- **Always on.** Keep session continuity (`session`), and use sub-agents that
  vary provider, model and effort (`orchestrate`). Use `tracker` when the
  project tracks work in GitHub issues.
- **Repository docs.** A repository's AGENTS.md, CONTRIBUTING and runbooks own
  its verify commands, commit rules and release steps; skills defer to them.
- **Git.** Implementation branches and linked worktrees are pre-authorised,
  with one writer per worktree. Merge through the repository's own workflow;
  an authorised merge prunes its own worktree and merged refs. A writer may
  rebase, rewrite and force-push with lease its own unmerged branches. The
  chair decides which other branches and worktrees to remove, unmerged ones
  included, once their work has landed, been superseded or been abandoned;
  record each removed tip and never remove one a live writer holds or one
  with uncommitted edits. Rewriting, deleting or pushing to a shared branch
  needs the user's authority.
- **Checks.** Locally, run only fast targeted checks: lint, type checks and the
  tests you touched. Full suites and release scans run in CI: push the branch,
  open the pull request and gate the merge on its CI run.
- **Memory.** Durable knowledge belongs in project docs. Private memory holds
  only preferences that apply across projects.
- **Provenant.** The core commands are `provenant run`, `status`, `output`, `steer`,
  `stop`, `retry`, `edit`, `templates`, `land`, `quota` and `trace`.
  `provenant help --all` lists the rest. Route answer-bearing work for other providers through the `orchestrate` skill and Fabric, which keeps full
  output in run files. Fabric identity is the working directory (worktrees
  share their repository's); `PROVENANT_AGENT_LABEL` splits inboxes.
- **Style.** Keep agent-to-agent, status and mechanical messages terse. Write
  for the user in the register the domain expects.
- **Shell.** The interactive shell is usually zsh: unquoted variables do not
  word-split, an unmatched glob aborts the command, and a pipeline reports only
  its last command's status. Check the output, not just the exit code, before
  trusting an empty result or a green run.

Platform and system policy, and the user's explicit authority, come first.
Project instructions may strengthen these rules but never broaden authority,
weaken safety or redefine cross-project memory.
