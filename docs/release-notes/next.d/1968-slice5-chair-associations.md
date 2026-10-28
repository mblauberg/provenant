## Additions
- Bind direct dispatches to explicit chair Fabric identities in a host-owned index. Nested launches and continuations inherit the sealed binding; conflicting proposals are refused. Missing associations, peer executions and graph-created runs remain unbound. Worker provider sessions never imply chair ownership. Part of #1968.
- Fabric advances to schema 9. Older binaries refuse upgraded stores; restore a pre-upgrade backup or forward-repair. Archive both `fabric.sqlite3*` and `host.sqlite3*` together after stopping store holders.
