# ADR 0011 — GitHub owns current work state

**Status:** Accepted 2026-07-16 (user, [issue
#156](https://github.com/mblauberg/provenant/issues/156)); supersedes [ADR
0006](0006-defer-backlog-contract.md); consistent with the issue-native scope
amendment in [ADR 0017](0017-specifications-own-non-derivable-intent.md)

## Context

ADR 0006 left project-local work maps and GitHub issues as joint current-work
owners. That dual narration drifted: the Console effort map described the work
as complete while the specification index described final verification as
pending, and repository branch-rule state differed between an effort map and
ADR 0001.

The document scope also needs to be explicit. `AGENTS.md` and `HARNESS.md` are
globally applied harness doctrine across projects. Under `docs/`, ADRs own
Provenant decisions and specifications own Provenant requirements;
`docs/efforts/` and `docs/handoffs/` are Provenant-meta navigation and temporary
continuity, not globally applied doctrine or current-work authority.

The repository declaration makes the current change scope/story home explicit.
For this repository it is the GitHub issue, so this decision remains the sole
current-work owner; project docs retain durable requirements and decisions.

## Decision

For Provenant work, the declared issue tracker and its workflow state are the exclusive
owners of current work state:

- Project Status owns workflow state.
- The issue owns the current owner, dependencies, scope-specific user gates and
  links to delivery pull requests.
- Specifications own requirements and acceptance criteria, without reporting
  implementation or verification state.
- ADRs own architectural and governance decisions, without reporting live
  repository or delivery state.
- Effort maps are curated route maps. They link specifications, ADRs, issues and
  pull requests but never restate current status, owner, dependencies or user
  gates.
- Handoffs are temporary continuity notes linked to an issue or run and removed
  or archived when consumed. They do not become durable work-state owners.

This is a direct ownership cutover. Git history preserves superseded narration;
no Markdown-to-GitHub synchronisation, backlog schema, cross-store migration or
god manifest is introduced.

## Consequences

- A reader follows the linked issue and its Project Status field for current
  work state instead of reconciling Markdown claims.
- Effort and specification indexes remain useful for discovery without needing
  status updates after each work transition.
- Repository-setting observations belong in their owning issue or pull request,
  while ADRs retain only the durable decision and authority boundary.
- ADR 0006's deferral is superseded: the harness still defines no generic
  backlog contract, but Provenant now has one unambiguous work-state owner.
