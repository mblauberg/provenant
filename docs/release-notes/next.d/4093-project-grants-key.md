## Breaking changes

### Lanes and settlement

- `domains.lanes.projects.<project_id>.capabilities` is now `domains.lanes.projects.<project_id>.grants`: the old word described both granted resources and adapter capabilities. `provenant install` rewrites `<state>/config.toml` once; there is no alias, so update any other tooling that writes the key. (#4093)
