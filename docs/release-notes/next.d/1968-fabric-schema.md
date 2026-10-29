## Breaking changes
### Fabric and MCP

- **Fabric moves to schema 9 on upgrade, and older binaries refuse the upgraded store.** Upgrade every host together. To go back, stop everything that holds the stores, archive `fabric.sqlite3*` and `host.sqlite3*` together, and restore a backup taken before the upgrade (#1968).
