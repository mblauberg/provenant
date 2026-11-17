## Fixes

### Pipelines and graphs

- Landing skips unreadable graph review sources and warns with their graph IDs. Corrupt checkpoints remain visible through status and tick reporting, including after rebuilding the pending index. (#3400)
