## Breaking changes
### Fabric and MCP

- **Fabric moves to schema 9 on upgrade.** Older binaries refuse the upgraded store, so upgrade every host together. To go back, restore a backup taken before the upgrade, or repair forward. Stop everything that holds the stores, then archive `fabric.sqlite3*` and `host.sqlite3*` together (#1968).
