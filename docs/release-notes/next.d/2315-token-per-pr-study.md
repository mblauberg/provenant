## Features
### Telemetry

- `provenant-telemetry` gains an offline token-per-merged-PR study reducer
  (`study::reduce`) and an example binary, with method and results template in
  `docs/telemetry/token-per-pr-study.md`. It reports `pending`,
  `inconclusive` or `comparable` and adds no counters. Any missing attempt or
  chair counter makes the verdict `inconclusive`, work is attributed to the
  cohort of its PR's merge time, and duplicate chair sessions are refused
  (#2315).
