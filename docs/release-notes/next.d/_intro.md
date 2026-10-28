This patch release fixes problems found while upgrading hosts to 1.2.0:
- The peer LaunchAgent restarts on the new binary after an upgrade.
- Writer attempts orphaned before a reboot can be settled instead of staying `running`.
- `lanes` shows when a lane's guardian has paused builds for disk pressure.

## Stability

There is no stability promise. Commands, flags, `--json` output, `config.toml`
keys, the state layout and Fabric MCP tool names and schemas can change in any
release, with no deprecation period or compatibility shims. Each release's notes
list its breaking changes. Upgrade every host together, since peers on different
versions are not supported.
