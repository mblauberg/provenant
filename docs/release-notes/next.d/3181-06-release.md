## Breaking changes
### CLI and install

- **`status --json` reports omitted graphs by state.** Read `omitted` as a map from state to count. Status and wait now share node digests with causes, branches, PR and CI observations, result paths and provider errors; retained external observations are labelled stale and missing observations unknown. (#3181)
