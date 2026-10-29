## Behaviour changes
### Fabric and MCP

- Plan nodes may declare an allow-list of patterns. A fragment instantiates only those patterns, with parameters validated against their typed schemas, and the pattern and effective parameters are recorded on the graph. Unlisted patterns, invalid parameters and widening attempts pause the graph, and the expanded nodes count against the host ceilings (#2212).
