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
Keep long lanes for coupled work that cannot be split.

```sh
provenant run --use plan --file brief.md --explain
provenant run --use batch --file approved-work.md
```

Inspect the explanation before submitting. Choose `explore` over named areas
when evidence is thin, `research` for dated primary sources, and `review` for
a pinned head. Use nodes with `over` for independent questions or slices,
`after` for dependencies and `inputs` for evidence. Templates live in bundled,
user, repository and inline layers; repository templates belong under
`.provenant/templates/`. See the [template runbook](../../docs/runbooks/presets.md).

Give every writer a separate worktree and literal write scope. Integrate slices
into one branch, then review its combined head. The [planner brief](references/planner.md)
owns slicing and tier choices; [brief and collect](references/dispatch.md)
owns the hand-off. Keep coupled writes with one owner.

Use `provenant status` to orient and `provenant output TARGET` to collect.
Use `steer` for a message, `stop` for a subtree, `retry` for unfinished work,
and `edit` for a graph patch. Launch by a requested tier such as `senior`;
the attempt receipt records the resolved `adapter/model@effort`. Inside a worker, `run --use TEMPLATE` adds a child
to the same graph, sharing budget, stopping and trace. Any agent may return a
patch; started attempts stay immutable.

Verify commits, changed paths, checks and actual routes against retained output.
A synthesis cannot certify a failed gate. Use deterministic checks where an
oracle exists and bounded repair with independent delta review otherwise.
HARNESS owns review floors and authority; the chair keeps user communication
and acceptance. Load [routing](references/routing-and-tiers.md) for tier and
independence decisions and [writer boundaries](references/lane-safety.md) for
confinement. Interactive observation belongs in the [Herdr runbook](../../docs/runbooks/herdr.md).
