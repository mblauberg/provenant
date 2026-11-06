---
name: orchestrate
description: "Use for template graphs, composites, parallel work, review panels and bounded repair loops. Not for single-owner work or coupled defects (diagnose). Session owns continuing goals and STOP missions."
---

# Orchestrate

Use `task` for one bounded assignment, `plan` when the shape is unsettled,
`batch` for parallel implementation and `deliver` for implementation through
landing. A composite is a template containing nodes; dotted names select
flavours such as `review.security` and `implement.fix`. Slice larger work into
lanes of about three acceptance items each, merged into one integration branch.


```sh
provenant run --route junior --file brief.md --explain
provenant run --use plan --file brief.md --explain
provenant run --use batch --file brief.md --set 'worktree="/absolute/project/.worktrees/slice"' --set 'write_scope=["src"]' --explain
```

Use `explore` over areas, `research` for primary sources and `review` for
a pinned head. Nodes use `over`, `after` and `inputs` for copies, dependencies
and evidence. Layers are bundled, user, repository and inline; project templates go under
`.provenant/templates/`. See the [template runbook](../../docs/runbooks/presets.md).

Register writer worktrees first with `provenant worktree create NAME --base BASE`.
Use separate worktree and write_scope bindings per slice; review the integrated head. The [planner brief](references/planner.md)
owns slicing and tier choices; [brief and collect](references/dispatch.md)
owns the hand-off. Keep coupled writes with one owner.

Orient with `provenant status`; collect with `provenant output TARGET`.
Use `steer` for a message, `stop` for a subtree, `retry` for unfinished work,
and `edit` for a graph patch. Use `--route junior` by default for implementation slices; raise the tier for design.
The receipt records the resolved `adapter/model@effort`. Inside a worker, `run --use TEMPLATE` adds a child
to the same graph, sharing budget, stopping and trace. Attached live children are
cancelled when their parent settles, including a successful exit. Use `--wait`
to collect their completion before exiting, or explicitly detach a child that
should outlive its parent. Any agent may return a patch; started attempts stay immutable.

Verify commits, paths, checks and routes against output. A synthesis cannot waive
a gate. Bound repair and independently review its delta.
HARNESS owns review floors and authority; the chair keeps user communication
and acceptance. Load [routing](references/routing-and-tiers.md) for tier and
independence decisions and [writer boundaries](references/lane-safety.md) for
confinement. See [Herdr](../../docs/runbooks/herdr.md) for observation.
