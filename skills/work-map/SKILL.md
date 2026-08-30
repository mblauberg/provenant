---
name: work-map
description: "Use when a multi-session effort needs one durable, curated route linking its specification and work items. Not for live status, ownership, dependencies, user gates or session handoffs; use the project's work tracker and session."
---

# work-map: the map for multi-session efforts

A `session` handoff carries one session's baton. Across many sessions or agents,
the work map preserves the stable route: one file per effort, read for durable
orientation and changed only when that route changes.

Read the repository's `Repository process` declaration first. If the declared
issue tracker has a parent issue with enough destination, route and invariant
detail, the parent tracker issue is the work map: link to it and do not create
a duplicate file. If the declaration selects `project-docs`, use its named map
or story home. A link-only file is allowed only as a fallback for an unavailable
tracker or a cross-tracker route. It links owners and never restates current
status, owner, dependencies or user gates.

## Optional link-only map

Create a file only for declared `project-docs`, or an unavailable/cross-tracker
fallback. If the parent issue has enough route detail, it is the canonical map
and no file is created. A docs fallback may use `docs/efforts/EFFORT-<slug>.md`
when authorised; otherwise return the route without writing.

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
- **Route entries are links, not task summaries.** Stable grouping/order is
  allowed; issue and pull-request prose carries changing detail.
- **Resume order:** declared scope/story owner → this map when it exists for
  fallback or cross-tracker route context → the claimed session handoff only.
  Never reconstruct the route from transcripts or piled-up handoffs.
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

When the declared tracker is unavailable, use the named project-docs home or a
link-only fallback; never invent rolling project state. Console and Herdr remain
optional. The portable [effort-map artifact](portable-workflow.v1.json) proves
only that a context object existed, not curated route state or a resumable
handoff. Retain canonical context separately; the runner validates its declared
fields, including `accepted_artifact_identity`.
