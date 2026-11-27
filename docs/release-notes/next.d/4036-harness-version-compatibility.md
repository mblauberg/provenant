## Additions
### Telemetry and providers

- Harness versions are assessed against `[[harness_compatibility]]` ranges (`provenant_range`, `recommended_range`), bundled empty and overlaid by host and project config. `policy.HARNESS_VERSION_UNSUPPORTED` and `policy.HARNESS_VERSION_BROKEN` warn by default at launch and in `check providers`. `providers.<id>.version_probe` turns the bounded `--version` read off, and `providers.<id>.version_probe_timeout_seconds` (default 4, 1-60) bounds it. (#4036, #4034)
