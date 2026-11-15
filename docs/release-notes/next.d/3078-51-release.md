## Fixes
### Pipelines and graphs

- Occupied graph concurrency caps warn and let eligible work run unless an explicit policy refuses it; depleted work takes the next eligible route without waiting for a quota reset. (#3078)
