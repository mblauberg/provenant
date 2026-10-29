## Additions

- Fabric graph plan nodes expand validated, bounded fragments with senior routes by default, scoped writer scheduling and the existing integrate delivery into one final pull request (#1996).
- A structured stopped-early handoff creates one continuation per writer on the same checkout, within a configurable graph budget. Graph status shows expansions, handoffs and remaining budgets (#1996).
- Refused planner fragments survive a resume, so limits can be extended. A malformed optional handoff warns without blocking delivery. Writer admission covers overlapping or unspecified scopes, and inactive recovery writers no longer block later fragments (#1996).
- Graph nodes can route through a pool by alias alone and carry readable task titles, which default to the node ID (#2003).
- Graph limits and budgets are configurable and can be extended when resuming a paused graph. Unknown usage is visible and can be set to pause the graph (#2004).
- Graph nodes record typed, bounded outputs and can select outputs from direct dependencies for their briefs. Graph status JSON gives SHA-256 digests of results and bounded excerpts. Selected outputs are delimited as untrusted data, and result paths appear in graph status but not in downstream briefs (#2005).
- A configurable `synthesise` node combines selected upstream outputs. Verdict and budget gates record decisions without an attempt, route failures to recovery nodes and keep their outcomes across checkpoint replay. A typed verdict needs a unique final standalone verdict line, and a malformed or repeated verdict fails the gates that consume it (#2006).
- `loop` nodes repeat a bounded body until its gate passes, carry outputs between rounds and keep writers on the same branch and worktree. Graph status shows each round and its outputs (#2007).
- An exhausted loop pauses for stop, extension or acceptance through explicit resume options, with an inbox decision when an originating session is configured. Extensions default to at most five extra rounds per decision and must fit the host limits. The three-node, two-round runbook example needs host depth 6, and extending it to four rounds needs depth and ceiling 12 (#2007).
