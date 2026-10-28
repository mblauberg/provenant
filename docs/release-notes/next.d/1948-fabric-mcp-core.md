## Behaviour changes

- Fabric MCP exposes a compact core catalogue, with graph, lease, task and
  event schemas available through discovery. Submit and wait in one bounded
  call, request detail when needed, and use returned operation IDs for retries.
  Output reads default to bounded result pages, and errors include recovery
  guidance.
