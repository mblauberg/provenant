## Fixes
### Lanes and runs

- OpenCode launches on macOS no longer fail confinement when `~/.config/opencode` exists: an exact read grant may name a directory, which the profile lists without reading its children. The macOS CI job now runs the provider tests. (#2793)
