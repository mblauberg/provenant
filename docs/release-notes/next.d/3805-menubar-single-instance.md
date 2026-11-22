## Fixes

### CLI and install

- The menu bar no longer starts twice after an upgrade. The bundled helper now
  lives in `Provenant Console.app/Contents/MacOS` and runs only from the
  `dev.provenant.quota-menubar` LaunchAgent; install removes the old login-item
  app and its launchd registration, and `provenant check install` warns while a
  leftover remains. A new helper replaces a live older instance at startup, so
  an upgraded binary takes over and the previous menu bar process quits. (#3805)
