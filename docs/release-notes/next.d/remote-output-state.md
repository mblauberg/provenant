## Fixes

- `output --host` now carries the attempt's `state` and `reason`, so a remote
  attempt that ended `uncommitted` shows its outcome and dirty paths (#1748).
