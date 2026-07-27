# Provenant: global agent instructions (all harnesses)

Depth lives in the adjacent harness constitution; read it before orchestration,
routing, delegation, memory or pane decisions. Skills live at
`$HOME/.agents/skills/<name>/` (Codex reads the installed `~/.codex/skills/`
mirror); a named skill (e.g. the `implement` skill) means read its `SKILL.md`,
which discloses its own references. Skill names are binding, not advisory.

- **Objective:** quality per user attention-hour. Verify, delegate and curate.
- **Sub-agents:** use them; vary model and effort per the harness constitution.
- **Memory:** durable knowledge belongs in project docs. Private memory holds
  cross-project preferences only.
- **Git:** implementation branches and linked worktrees are pre-authorised,
  including parallel work; one writer per worktree. An authorised merge prunes
  its own worktree and merged refs; other deletion, force-removal, history
  rewrites and shared-branch pushes outside authorised merges need user authority.
- **Fabric trust:** before first use, explicitly trust only the exact canonical
  repository root (or current non-Git directory) with
  `provenant fabric workspace trust`; never trust a parent, wildcard, home or
  sibling collection. If no seat exists, call `fabric_bootstrap`. On
  `WORKSPACE_NOT_TRUSTED`, run its exact recovery command and retry
  `fabric_bootstrap`; the same MCP connection exposes normal tools.
- **CLI:** use `provenant help` for discovery; route answer-bearing external
  work through the `orchestrate` skill and Fabric.
- **Style:** terse for inter-agent, mechanical, and status traffic;
  domain-appropriate user prose. Load the `caveman` skill only when requested.

Platform/system policy and explicit user authority lead. Nearest project
instructions may strengthen, but never broaden authority, weaken safety or
redefine cross-project memory. Only the user may grant a one-run worktree-path
exception.
