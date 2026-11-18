## Fixes

### Telemetry and providers

- On macOS, a timed-out provider updater no longer fails its check with a spurious EPERM while cleaning up its private process group. The ownership census now counts a member that is exiting or was reaped mid-kill as unsignalable, so only a live member keeps the EPERM an error. (#3380)
- The provider node-prefix tests derive expected paths from the canonicalised temporary directory, so they pass on macOS where `/var` resolves to `/private/var`. (#3380)
