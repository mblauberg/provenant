## Breaking changes
### Fabric

- **The Fabric MCP server is removed.** `provenant fabric mcp`, the `fabric.mcp_server` setting and the `fabric` entry the installer wrote into harness configuration are gone; every harness Provenant drives has a shell, and `provenant fabric` covers every tool. Use `provenant fabric` from the lane's shell, and delete any `fabric` MCP entry left in your own harness configuration, because install no longer touches or removes it. (#3078)
