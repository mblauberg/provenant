## Fixes
### Fabric and MCP

- Fabric MCP accepts empty text in chair summary updates (`now`, `next`, `risk` and `needs_you` items), matching the CLI, while empty log entry markdown is still refused. Structured CLI error details now carry the CLI error data once, rather than twice (#2150).
