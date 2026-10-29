## Fixes
### Telemetry and providers

- `telemetry stats` and graph node usage no longer treat every attempt as partially priced because providers report no separate `reasoning` count. Only a missing price or a missing priced count leaves a cost a lower bound, and an empty report names the project it read (#2174).
