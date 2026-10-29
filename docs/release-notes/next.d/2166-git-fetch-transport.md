## Behaviour changes
### Fabric and MCP

- Peer code transport fetches committed heads with `git fetch` into `refs/provenant/transport/<operation>/` refs, with `fsckObjects` and a forced `index-pack`, instead of writing and verifying bundle files. Commits over 64 MiB now transport; corrupt, missing, duplicate and malformed objects are still refused (#2166).
- The macOS peer agent no longer caps concurrent requests at 16 or answers a busy frame; every accepted request is relayed (#2166).
