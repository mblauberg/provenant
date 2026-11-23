## Additions
### Fabric

- The bundled `smoke` graph now declares `retry-route` and `steer-route` probes
  beside `read`, `review` and `write`, on the same free-capable `junior` tier as
  the rest of the graph. Both are overridable like the other nodes:
  `--set smoke-retry=false` and `--set smoke-steer=false` skip them, and
  `upgrade --verify --dry-run` lists them by node id. A failing probe is
  reported by name during `upgrade --verify`, as any other smoke node is.

  The probes check that a smoke run has a route for the retry and steer paths;
  they do not retry a failed node or steer a running lane. Driving those
  operations from `upgrade --verify` would need the verifier to launch a graph
  that fails on purpose and then invoke the controls, which this change does not
  do. (#3502)
