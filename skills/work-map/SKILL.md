---
name: work-map
description: "Use when a multi-session effort needs one durable, curated route linking its specification and work items. Not for live status, ownership, dependencies, user gates or session handoffs; use the project's work tracker and session."
---

# work-map: the map for multi-session efforts

A `session` handoff carries one session's baton. Across many sessions or agents,
the work map preserves the stable route: one file per effort, read for durable
orientation and changed only when that route changes.

The project's work tracker owns live work state. In Provenant, GitHub issues
own the current owner, dependencies and user gates, while Project Status owns
workflow state. An effort map links those owners and never restates current
status, owner, dependencies or user gates.

## The map file

Use the project's canonical effort file. If no owner exists, propose
`docs/efforts/EFFORT-<slug>.md` only when project-write authority allows it;
otherwise return the map without writing. Structure:

```markdown
# EFFORT: <name>

## Destination
What the route is intended to deliver. Link the owning specification.

## Route
- [Programme issue](https://example.invalid/issues/1)
- [Related delivery](https://example.invalid/pull/2)

## Invariants
- [Governing decision](https://example.invalid/decisions/1)
```

## Rules

- **Link, never restate live work state.** Do not add status fields, task
  checkboxes, completion claims, owner names, dependencies, blockers or user
  gates. Readers follow the linked issue and its Project Status field.
- **Route entries are links, not task summaries.** Stable grouping and ordering
  are allowed; issue and pull-request prose carries changing detail.
- **Resume order:** project state file → issue and Project Status → this map for
  route context → the claimed session handoff only. Never reconstruct the route
  from transcripts or piled-up handoffs.
- **Handoffs stay temporary.** They carry continuity for an active session or
  run, are not linked as route state, and are removed or archived when consumed.
- **One map writer.** Parallel workers write namespaced continuity artifacts;
  one chair updates the shared route after checking the tracker.
- Archive a route map under the project's archival policy only when the
  owning issue records that disposition.
- Validate an authored map with
  `scripts/validate_work_map.py <EFFORT-file>` before handoff.

## Red flags

- `Status:`, task checkboxes or an "Updated" freshness claim → delete them and
  link the work tracker.
- Map restates a specification, decision or issue → link, don't copy.
- A handoff appears as route state → keep it in the temporary continuity layer.
- Re-planning changes accepted scope → return to `scope`, then update stable
  route links after the owning issue records the decision.

## Adapter-absent path

When GitHub is unavailable, use the project's designated canonical work
tracker; a project with no tracker names one canonical work-state owner in
its state file before any route map links it. Console and Herdr remain
optional. The portable [effort-map artifact](portable-workflow.v1.json) proves
only that a context object existed. Retain and identify the canonical context
separately; this output is not itself curated route state or a resumable
handoff. The runner validates the contract's declared context fields, including
`accepted_artifact_identity`, before emission.
