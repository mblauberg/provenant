## Breaking changes
### Fabric and MCP

- Removed the `provenant bench` verb (`scripts/bench` remains and is now the entry point for the bench runbook), the `provenant fabric landing` verbs (`push`, `reconcile`, `acquire`, `renew`, `verify`, `release`, `wait`), the `landing_*` operations of the `fabric_lease` MCP tool (work-claim operations remain), the `PROVENANT_LANDING_TOKEN` variable, and the `release-fixtures.txt` path pinning in `provenant check release`, which now reports every matching line (#3226).
