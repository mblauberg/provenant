## Fixes

### Telemetry and providers

- On macOS, a timed-out provider updater no longer fails its check with a spurious EPERM while cleaning up its private process group. The group belongs to the executor, so EPERM means nothing live remains to signal even when the last orphan exits mid-kill, and the lease is released normally. (#3380)
- The provider node-prefix tests derive expected paths from the canonicalised temporary directory, so they pass on macOS where `/var` resolves to `/private/var`. (#3380)
