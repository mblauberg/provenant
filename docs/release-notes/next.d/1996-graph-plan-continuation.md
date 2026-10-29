## Additions
### Pipelines and graphs

- Fabric graph plan nodes expand validated, bounded fragments into one final pull request, and a stopped-early handoff creates one continuation per writer within a configurable budget (#1996).
- Graph limits and budgets are configurable, and unknown usage is visible and can pause the graph (#2004).
- Graph nodes can route through a pool by alias and carry readable task titles (#2003).
- Graph nodes can pass selected outputs to later nodes, and `status` shows a short excerpt of each (#2005; see [graphs](../../specs/fabric.md#bounded-graphs-and-route-fallback)).
- A `synthesise` node combines selected upstream outputs, and verdict and budget gates route failures to recovery nodes (#2006).
- `loop` nodes repeat a bounded body until their gate passes, and an exhausted loop pauses for stop, extension or acceptance (#2007; see [bounded loops](../../specs/fabric.md#bounded-loops)).
