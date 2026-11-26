## Fixes

### Status

- `provenant status NAME` no longer returns `NOT_FOUND` when the graph lives in another registered project. On a miss in the caller's project it searches the state root's bindings: a single match is read through the owning project and reported with a `CROSS_PROJECT_TARGET_FOUND` note naming that project, while several matches refuse with `AMBIGUOUS_TARGET` listing them. Reads inside a lane keep the existing chair-session read scope. (#4073)
