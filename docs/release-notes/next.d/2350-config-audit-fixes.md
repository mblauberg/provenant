## Fixes

- A `domains` value that is not a table, such as `domains = false`, now refuses to load for the settings that control a boundary, such as `[domains.run]`, instead of silently using their defaults. The error names the config file and the `domains` key (#2350).
- A single `config apply --file` patch can now create a calibration entry, such as `domains.quota.calibration.<account>.five_hour` with its `expires` timestamp. `config schema` lists the calibration fields with their 0 to 100 bounds (#2350).
- A result fallback that the effective routing policy drops is now reported once for the route being launched, and only when it is actually dropped. A rule overlay that makes the fallback valid no longer produces a false "dropped" warning (#2350).
