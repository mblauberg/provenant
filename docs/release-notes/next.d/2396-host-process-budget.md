## Fixes

- Lane admission and detached workers now respect the user's process limit. A lane waits, and says `host capacity: user process count N of LIMIT`, when fewer than a tenth of the per-user process limit (at least 64) remain, and a detached worker refuses to start. Before, a host near the limit failed later with misleading errors such as "provider protocol is malformed" ([#2396](https://github.com/mblauberg/provenant/issues/2396)). Production admission now uses the same probe; Linux counts threads by real user ID, and process-capacity refusals remain visible in launch status.
