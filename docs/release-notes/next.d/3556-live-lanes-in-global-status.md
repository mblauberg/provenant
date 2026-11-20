## Fixes

### Pipelines and graphs

- `provenant status` no longer hides a settled graph whose lanes are still running. The global listing now leads with the live-lane count from the same attempt presence digests as per-name status, and `--json` exposes it as `live_lanes`. (#3556)
