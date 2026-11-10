## Breaking changes
### Fabric MCP server

- The Fabric MCP server is now opt-in. A default install writes no `fabric` MCP entry, an upgrade removes the unchanged entry an earlier release wrote (edited and foreign entries stay), and lanes launch without it. Clients without a shell turn it on with `provenant config set fabric.mcp_server true` and reinstall; the `provenant fabric` and top-level commands cover every tool. (#3078)
