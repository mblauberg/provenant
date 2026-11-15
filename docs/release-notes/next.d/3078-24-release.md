## Behaviour changes
### Lanes and settlement

- Lanes share the host Cargo home, registry and Git downloads, and can reuse matching warm build targets, including in nested workspaces. With sandboxing off, lanes can write host `~/.cargo/config`; an enforcing sandbox grants read-only access to that configuration. Cargo credentials remain protected and build output stays private. Set `domains.fabric.lanes.cargo_source_writes = false` for read-only source caches and offline Cargo. (#3078, #3207, #3274)
