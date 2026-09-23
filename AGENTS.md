# Provenant: global agent instructions (all harnesses)

Depth lives in the harness constitution, `HARNESS.md`; read it before
orchestration, routing, delegation or memory decisions.

- **Objective:** quality per user attention-hour. Verify, delegate and curate.
- **Sub-agents:** use them; vary model and effort per the harness constitution.
- **Memory:** durable knowledge belongs in project docs. Private memory holds
  cross-project preferences only.
- **Git:** implementation branches and linked worktrees are pre-authorised,
  including parallel work; one writer per worktree. An authorised merge prunes
  its own worktree and merged refs; other deletion, force-removal, rewrites and
  shared-branch pushes need user authority.
- **Fabric:** messages, shared tasks, activity and a dispatch/batch front door
  for agents in one project. Execution tools delegate to the orchestration
  owners and keep full output in run files. Identity derives from the working
  directory; registered worktrees share their repository's project while
  keeping their own cwd. `AGENT_FABRIC_LABEL` gives several agents of one provider separate
  inboxes.
- **CLI:** use `provenant help` for discovery; route answer-bearing external
  work through the `orchestrate` skill and Fabric. opencode reads only inside
  its own working directory; copy any input it needs there first.
- **Style:** terse for inter-agent, mechanical and status traffic;
  domain-appropriate user prose. Load the `caveman` skill only when requested.
- **Shell:** the interactive shell is commonly zsh, whose quoting and
  word-splitting differ from a script's; see
  [`docs/runbooks/shell-pitfalls.md`](docs/runbooks/shell-pitfalls.md) before
  trusting a "no results" or a green exit status.

Platform/system policy and explicit user authority lead. Nearest project
instructions may strengthen, but never broaden authority, weaken safety or
redefine cross-project memory.
