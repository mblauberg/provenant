## Fixes
### Fabric and MCP

- Lanes send Fabric messages and task updates through their host, under the identity of their own attempt. They can read Fabric state without write access to its database or WAL files (#2285).
