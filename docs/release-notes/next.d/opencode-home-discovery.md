## Fixes

- OpenCode lanes no longer see the entries of `~/.claude` or `~/.codex`. Their
  discovery probes along the working directory now stop below the user's home,
  so they grant nothing at or above it. This also stops lanes in projects
  under `/Users/<name>` being refused before launch as too broad.
- Invalid-path refusals now read "path is not a permitted absolute, normalised
  path", which also fits paths refused for their scope, name or breadth.
