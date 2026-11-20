## Breaking changes

### Lanes and settlement

- The `build_admission` port is removed. Action: move a repository wrapper to `build.strategy = "command"` with `build.command`. A leftover key warns as unknown. (#3655)
