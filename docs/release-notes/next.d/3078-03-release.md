## Breaking changes
### CLI and install

- **Update scripts and integrations for the 2.0 command surface.** Removed commands and options exit 2 with their replacement; there are no aliases. Use the [consumer migration guide](../../runbooks/using-provenant-2-from-another-repository.md) for the complete mapping, configuration changes and guidance for consuming repositories. (#3078, #3263)
- **The Fabric MCP server is removed.** `provenant fabric mcp` exits 2; lanes call `provenant fabric` from their shell. Install removes the Fabric MCP registrations it made. (#3078)
