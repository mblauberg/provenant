## Fixes

- `telemetry stats` and graph node usage no longer treat every attempt as
  partially priced because providers report no separate `reasoning` count:
  reasoning is priced at the output rate and counted inside output, so only a
  missing price or a missing priced count leaves a cost a lower bound. An empty
  report names the project it read.
