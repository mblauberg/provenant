## Behaviour changes

- Fabric MCP exposes a compact core catalogue. Graph, lease, task and event schemas are available through discovery.
- Submit and wait in one bounded call, request detail when needed, and use the returned operation IDs for retries. Output reads default to bounded pages, and errors include recovery guidance.
