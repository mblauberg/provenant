## Fixes

### Accounts, quota and routing

- Cursor registration and quota probes share one typed identity reader and no longer require the external SQLite command. Unavailable identity data remains an explicit failure. (#3078)
