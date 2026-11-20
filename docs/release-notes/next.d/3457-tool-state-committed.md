## Fixes
### Lanes and settlement

- Settlement now warns when a writer's commits add or modify tool state at the repository root. Paths whose first segment is `.local`, `.config` or `.cache` are named in the warning, so a lane that accidentally commits a tool's device identity under `.local/state/` no longer goes unnoticed. Ordinary dot paths such as `.github/x` and `src/.config` are unaffected. This is a warning: the lane still settles. An operator who wants a hard stop can set `policy.LANE_TOOL_STATE_COMMITTED` to `refuse`; an unreadable diff also warns and never blocks settlement. (#3457)
