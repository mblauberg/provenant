## Fixes
### Lanes and settlement

- macOS lanes can initialise, query and cleanly stop PostgreSQL for local database checks without failing on shared-memory or signal permissions. Writer and read-only lanes share this support; database files still use their existing writable roots and signals to processes outside the lane remain denied (#2450).
