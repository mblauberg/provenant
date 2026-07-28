# Review branch (default, read-only)

Review the user-facing result, rather than the diff alone. Source stays
read-only: no `Write`, `Edit`, or `NotebookEdit` calls. Write only a report
path explicitly assigned by the user or enclosing run; otherwise return
findings in chat. Builds may run only when proven no-write or configured to
an assigned isolated output/cache path. Browser submit/send/purchase/delete/
account actions are outside review authority.

A request that starts as review and drifts toward "just fix it while you're
in there" stays on this branch. Surface the fix as a finding with a route to
`implement` + this skill's design/make branch; do not cross into a write
without an explicit authority grant naming `implement` (or this skill under
`implement`) as action-owner.

## Evidence plan

Define the surface, supported users/journeys, viewports, inputs, states and
known product/design intent. Inspect complete affected components, callers,
tokens and runtime boundaries. Select proportionate evidence:

- source/component and design-token inventory;
- existing production-build artifacts, or a build isolated to an assigned
  output/cache path, for representative desktop, tablet and mobile states;
- accessibility tree, automated checks, keyboard journey, visible/unobscured
  focus, zoom/reflow, reduced motion and forced colours;
- loading, empty, partial, permission, validation, network/server failure,
  offline/retry, destructive confirmation, long/localised and RTL cases;
- console/network errors and measured performance evidence where relevant;
- independent judgement of hierarchy, information architecture,
  [cognitive load](cognitive-load.md), task clarity, copy, brand fit and
  visual craft;
- `scripts/detector/` (see root SKILL.md) for automated antipattern and
  contrast findings — run it before or alongside manual inspection, never as
  a substitute for it.

Use `playwright` for terminal browser evidence, `web-stack-conventions` for
current WCAG/Lighthouse facts and `react-performance` for measured React
claims. Those overlays supply evidence; this branch owns the UI-review
workflow but not a second finding-contract schema.

Record each selected lane as `tested`, `failed`, `not tested` or
`not applicable`, with a command, artifact, route/state/viewport or source
anchor. A screenshot, source scan, accessibility tree, automated scanner or
Lighthouse run alone cannot certify WCAG or field performance. Unknown is not
passing; heuristic judgement is not a standards result.

## Findings

Findings use the `code-review` skill's canonical finding contract — severity,
provenance, mechanism, impact, fix and evidence fields. Do not restate that
schema here; load it. This branch adds only UI-specific reporting duties on
top of it:

- report unsupported or sensitive surfaces and retained artifacts;
- return `clean within tested coverage` when nothing survives, followed by
  material untested lanes.

Fixes route to this skill's design/make branch under `implement`; re-review
remains independent and source-read-only.
