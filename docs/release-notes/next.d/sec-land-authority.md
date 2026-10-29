## Fixes
### Pipelines and graphs

- An inbox answer no longer approves a graph land. A tier that asks posts its request, naming `provenant land N --head SHA`, and succeeds once it sees the pull request merged at the reviewed head; `hold`, or any other answer, fails the node. Lanes can write the Fabric store and configured peers can answer, so neither can approve a merge (see [land nodes](../../specs/fabric.md#land-nodes)).
- A land node accepts review evidence only from an agent attempt outside the writer's chain. A verdict gate on CI's own `PASS`, or on any gate, loop or decision output, no longer lands.
- A repository preset layer can no longer lower `land_tier` or turn on `land` beyond user configuration. Each land node takes the stricter tier, and a land your configuration turned off takes `terminal`; `--set` and `--params-file` still apply.
- A planner fragment can no longer choose an integration branch. Integrate delivery is refused unless the graph names an `integration_branch`, and the destination is always that branch.
- A graph land finds its pull request by head branch, requiring one same-repository match, so a numeric branch such as `123` no longer resolves to pull request #123.
