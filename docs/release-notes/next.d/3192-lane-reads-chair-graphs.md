## Fixes
### Fabric

- A lane can now read the `status`, `output` and `trace` of any graph launched
  by its own chair session, so a repair lane sees the chair's full review output
  instead of working from its brief. The chair session sealed on each graph's
  custody record decides the scope: no new launch flag or per-graph grant. A
  graph from another chair session or another project still reads `not_found`,
  and steer, stop and retry stay confined to the lane's own graph. (#3192)
