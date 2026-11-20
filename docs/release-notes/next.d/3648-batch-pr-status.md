## Fixes
### Pipelines and graphs
- `pr status` reports a member whose head is contained in an open batch's head as superseded, shows the batch's CI and attributes the batch's changed files to the member that last touched them; a member whose head moved after batching is flagged. (#3648)
