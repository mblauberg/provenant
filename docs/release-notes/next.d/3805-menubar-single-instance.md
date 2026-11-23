## Fixes

### CLI and install

- The menu bar no longer starts twice after an upgrade. The bundled helper now
  lives in `Provenant Console.app/Contents/MacOS` and runs only from the
  `dev.provenant.quota-menubar` LaunchAgent; install removes the old login-item
  app and its launchd registration once the installed app already runs that
  helper. A kept app that predates the helper may still be launched through the
  nested bundle, so install leaves that bundle alone and `provenant check install`
  warns instead of refusing while a leftover remains. A new helper replaces a
  live older instance at startup, so an upgraded binary takes over and the
  previous menu bar process quits: it signals only a PID still running this app's
  executable, writes the marker with a temporary file and rename, and re-reads it
  after a short delay so the loser of a concurrent start exits. (#3805)
- `provenant check install` now names `provenant upgrade` (or manual bundle and
  launchd removal) for a leftover menu login item; `provenant install` never
  removed it. (#3805)
