## Additions
### Pipelines and graphs

- Fabric graph plan nodes expand validated, bounded fragments into one final pull request, with senior routes by default and scoped writer scheduling (#1996).
- A structured stopped-early handoff creates one continuation per writer on the same checkout, within a configurable graph budget. Graph status shows expansions, handoffs and remaining budgets (#1996).
- Refused planner fragments survive a resume, so limits can be extended. Writer admission covers overlapping or unspecified scopes, and inactive recovery writers no longer block later fragments (#1996).
- Graph nodes can route through a pool by alias alone and carry readable task titles, which default to the node ID (#2003).
- Graph limits and budgets are configurable and can be extended when resuming a paused graph. Unknown usage is visible and can pause the graph (#2004).
- Graph nodes record typed, bounded outputs that later nodes can select for their briefs. Graph status gives SHA-256 digests and bounded excerpts; credential-redacted excerpts may not hash to the digest (#2005; see [graphs](../../specs/fabric.md#bounded-graphs-and-route-fallback)).
- A `synthesise` node combines selected upstream outputs, and verdict and budget gates record decisions and route failures to recovery nodes (#2006; see [graphs](../../specs/fabric.md#bounded-graphs-and-route-fallback)).
- `loop` nodes repeat a bounded body until its gate passes, and an exhausted loop pauses for stop, extension or acceptance through explicit resume options (#2007; see [bounded loops](../../specs/fabric.md#bounded-loops)).
