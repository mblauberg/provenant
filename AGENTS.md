# Provenant: global agent instructions (all harnesses)

The harness constitution, `HARNESS.md`, holds the depth behind these rules.
Routine work and native sub-agents do not need it. Read it before dispatching
to another provider, deciding a risk tier or an authority question, or
changing memory policy.

- **Objective.** Optimise quality per hour of the user's attention. Verify
  your work, delegate where that saves attention, and curate what you keep.
- **Sub-agents.** Use them, and vary model and effort as the constitution
  describes.
- **Memory.** Durable knowledge belongs in project docs. Private memory holds
  only preferences that apply across projects.
- **Git.** Implementation branches and linked worktrees, including parallel
  ones, are pre-authorised, with one writer per worktree. An authorised merge
  prunes its own worktree and merged refs. Any other deletion, force-removal,
  history rewrite or push to a shared branch needs the user's authority.
  Merge authority is repository-based: agents merge through the repository's
  own workflow.
- **Fabric.** Messages, tasks, activity and dispatch for a project's agents;
  full output stays in run files. Identity is the working directory (a worktree
  shares its repository's project); `PROVENANT_AGENT_LABEL` splits inboxes.
- **Checks.** Heavy checks, such as release scans and full test
  suites, run in GitHub Actions rather than on a contributor's or agent's
  machine. Push the branch, open the pull request and let CI run them.
  Locally, run only fast targeted checks: static lint, type checks and the
  tests you touched. Gate each merge on the pull request's CI run.
- **CLI.** Run `provenant help` to discover commands. Route answer-bearing
  external work through the `orchestrate` skill and Fabric.
- **Style.** Keep traffic between agents, and mechanical and status messages,
  terse. Write prose for the user in the register its domain expects.
- **Shell.** The interactive shell is usually zsh, whose quoting and
  word-splitting differ from a script's. Read
  [`docs/runbooks/shell-pitfalls.md`](docs/runbooks/shell-pitfalls.md) before
  trusting an empty result or a green exit status.

Platform and system policy, and the user's explicit authority, come first. The
nearest project instructions may strengthen these rules but never broaden
authority, weaken safety or redefine cross-project memory.
