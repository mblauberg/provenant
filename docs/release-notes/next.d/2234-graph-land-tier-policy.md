## Additions
### Pipelines and graphs

- A graph `land` node merges a writer's pull request through `provenant land` once its review and CI gates pass. By default, routine and substantial tiers land automatically and crucial asks first; set `graph.land`, per project if needed, to change this. The checkpoint records the policy, where it came from and who authorised the merge (#2234; see [land nodes](../../specs/fabric.md#land-nodes)).
