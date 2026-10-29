## Behaviour changes
### Fabric and MCP

- Fabric MCP exposes a compact core catalogue. Graph, lease, task and event schemas are available through discovery (#1948).
- Fabric MCP calls can submit and wait in one bounded call. Output reads default to bounded pages, and errors include recovery guidance. Use the returned operation IDs for retries (#1948).
