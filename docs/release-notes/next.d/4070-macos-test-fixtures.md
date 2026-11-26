## Fixes

### Pipelines and graphs

- Lane-side graph calls no longer report `relay_peer_refused` when macOS's scoped process walk fails or succeeds without the caller's parent chain. The relay seeds the scoped walk with the caller's exact identity and uses the documented full-census backstop whenever the snapshot is inconclusive, so a healthy host guardian is not mistaken for a replaced socket owner. (#4070)

### Tests

- Socket fixtures use the short canonical temporary root, so `SSH_AUTH_SOCK` and relay sockets stay inside the platform `sun_path` limit when `TMPDIR` is a long sandbox alias. An APFS filesystem that cannot create a non-UTF-8 name now reports a numbered, reasoned skip instead of failing, while a filesystem-independent test still pins the JSON-safe rendering of non-UTF-8 origins. The macOS native CI batch runs these fixtures on every batch so the behaviour stays pinned. (#4070)
