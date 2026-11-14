## Breaking changes
### CLI and install

- **`wait --json` emits settlement events before its final result.** Read the final JSON record for the command envelope, or consume each event as it arrives. `--any` returns on settlement, including a node finishing within a running graph; a node starting does not end the wait. (#3078)
- **`status --json` reports omitted graphs by state.** Read `omitted` as a map from state to count. Status and wait now share node digests with causes, branches, PR and CI observations, result paths and provider errors; retained external observations are labelled stale and missing observations unknown. (#3181)
