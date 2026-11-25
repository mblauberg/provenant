## Fixes

### CLI and install

- `status --full` gathers the run store, admission holders, worktree index and process census once per listing instead of once per graph, so a fleet-wide page no longer repeats the same host-wide scans for every graph. (#3976)
- `retry N` now infers the only node of a settled one-node graph instead of refusing with `provenant retry N/NODE`; a wider graph still needs an explicit node. (#3976)
- A stale `run review --head SHA` refusal now names `--head`, says the supplied head is not the pull request's head and shows both SHAs with a repair that names the pinned head once the push is visible, instead of the generic "check the named input field and retry". (#3976)
- `output` on a settled lane that already carries a result, including a failed lane, no longer appends the graph's failure rows, so a completed review lane no longer ends with stale "unreadable" attempt status lines; unsettled work and lanes without a result keep their diagnostics. (#3976)
