## Breaking changes
### Fabric and MCP

- `provenant fabric task`, the `fabric_task` MCP tool and the `--task` link on `fabric mail send` are removed. Track work in GitHub issues and graph nodes; mail, events, claims and landing are unchanged. Opening a Fabric store upgrades it and drops the task board, its events and receipts and any message task links, after which older releases refuse the store as newer. Until every peer host is upgraded, an older peer's task requests are refused and its stored task events cannot be read (#3082).
