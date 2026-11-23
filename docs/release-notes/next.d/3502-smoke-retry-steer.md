## Additions
### Fabric and install

- The bundled `smoke` graph now exercises the retry and steer paths with two
  read-only nodes on the cheapest (`utility`) routes. Both are overridable like
  the other nodes: `--set smoke-retry=false` and `--set smoke-steer=false` skip
  them, and `upgrade --verify --dry-run` lists them by node id alongside `read`,
  `review` and `write`. A failing retry or steer node is reported by name during
  `upgrade --verify`, as any other smoke node is. (#3502)
