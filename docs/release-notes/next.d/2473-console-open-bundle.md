## Fixes
### Install

- `Provenant Console.app` now nests the menu bar app as `Provenant Menu.app` (`dev.provenant.menu`, no Dock icon), signed helper first, so `provenant console open` shows the console window instead of only activating the agent. `provenant upgrade` migrates the existing menu bar LaunchAgent to the nested helper and restores the previous app and agent if the restart fails (#2473).
- `provenant console open` opens the app by bundle identifier and reports a repair hint when no console process is running, instead of printing "Opened" (#2473).
- Installing or upgrading always boots the host tick LaunchAgent out and back in, so launchd no longer kills `dev.provenant.tick` with a Launch Constraint Violation after `~/.local/bin/provenant` is replaced (#2469).
