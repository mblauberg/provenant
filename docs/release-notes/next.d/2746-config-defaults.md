## Breaking changes
### CLI and install

- **Retired review knobs and the `rigorous` profile are no longer accepted.** Remove them from `config.toml` and select `balanced` or `economy` (#2746).
- **Duplicate `[knobs] domains.routing.*` entries are no longer accepted.** Move routing settings under `[domains.routing]` or remove them with `provenant config migrate` (#2746).
- **The test-only `confinement.census_host_list_complete` knob is retired.** Run `provenant config migrate` to remove it; native acceptance uses `PROVENANT_CENSUS_HOST_LIST_COMPLETE` (#2746).
