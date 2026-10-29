## Behaviour changes
### Fabric and MCP

- Local stores now set one SQLite busy timeout when they open and wait inside SQLite instead of retrying each call under a per-call budget. A contended store waits up to `domains.fabric.limits.store_busy_timeout_ms` (30 seconds by default; 0 fails fast) before it reports contention, and the stores' advisory locks share that bound. Hook delivery still stops at its own deadline (#2163).
