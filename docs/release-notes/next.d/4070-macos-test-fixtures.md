## Fixes

### Pipelines and graphs

- Lane-side graph calls no longer report `relay_peer_refused` when a macOS process census is unavailable or inconclusive. The relay proves ancestry from the kernel-reported parent chain with exact live identities and stable parent links, without relying on process start timestamp ordering. (#4070)
- Isolated macOS lanes can verify their host guardian using public kernel process metadata when the sandbox denies libproc access, while retaining the ancestry check. (#4070)

### Tests

- Socket fixtures use the short canonical temporary root, so `SSH_AUTH_SOCK` and relay sockets stay inside the platform `sun_path` limit when `TMPDIR` is a long sandbox alias. An APFS filesystem that cannot create a non-UTF-8 name now reports a numbered, reasoned skip instead of failing, while a filesystem-independent test still pins the JSON-safe rendering of non-UTF-8 origins. The macOS native CI batch runs these fixtures on every batch so the behaviour stays pinned. (#4070)
