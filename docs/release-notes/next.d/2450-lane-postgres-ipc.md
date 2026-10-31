## Fixes
### Lanes and settlement

- macOS lanes can initialise and run PostgreSQL for local database checks without failing on shared-memory permissions. Writer and read-only lanes share this support; database files still use their existing writable roots (#2450).
