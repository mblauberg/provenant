## Fixes

- Fabric MCP accepts empty text in chair summary updates (`now`, `next`, `risk` and `needs_you` items), matching the CLI. Empty log entry markdown is still refused. Structured CLI error details carry the CLI error data once, rather than twice (#2150).
