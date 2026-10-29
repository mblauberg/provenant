## Behaviour changes
### Fabric and MCP

- Fabric run listings return compact attempt summaries, newest queued first. Follow `next_after` to page towards older attempts, and use `op: status` for the full record, including usage and execution evidence (#1937).
