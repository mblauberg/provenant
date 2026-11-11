## Breaking changes
### Fabric and MCP

- Removed the `provenant bench` verb (`scripts/bench` remains and is now the entry point for the bench runbook), the `provenant fabric landing` verbs (`push`, `reconcile`, `acquire`, `renew`, `verify`, `release`, `wait`), the `landing_*` operations of the `fabric_lease` MCP tool (work-claim operations remain) and the `PROVENANT_LANDING_TOKEN` variable (#3226).
