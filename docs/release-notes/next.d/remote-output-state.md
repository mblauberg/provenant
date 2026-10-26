## Fixes

- `output --host` now carries the attempt's `state` and `reason`, so a remote
  attempt that ended `uncommitted` shows its outcome and dirty paths (#1748).
- The peer protocol is now version 4. Hosts on different versions refuse each
  other with an "upgrade both" error, so upgrade every host together (#1748).
