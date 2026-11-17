## Fixes

### Pipelines and graphs

- Landing skips unreadable graph review sources and includes warnings with their graph IDs in the structured landing report. Corrupt checkpoints remain visible through status and tick reporting, including after rebuilding the pending index. (#3400)
