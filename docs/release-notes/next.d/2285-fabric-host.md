## Changes
### Fabric and MCP

- Lane MCP mutations for mail, tasks, notes and advisory claims are relayed through the host. Lane CLI `fabric ... send` remains refused (#2285).

## Fixes
### Fabric and MCP

- Lanes send Fabric messages and task updates through their host, under the identity of their own attempt. They can read Fabric state without write access to its database or WAL files (#2285).
