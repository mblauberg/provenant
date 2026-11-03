## Additions
### CLI and install

- `status --wait` waits on lanes, graphs and PRs together, returning on the first observed transition. Use `--until all` to wait for every target or `--follow --json` to stream changes; timeout and source intervals are configurable under `[domains.wait]` (#2721).
