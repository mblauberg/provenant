## Fixes

### Pipelines and graphs

- Landing skips unreadable graph review sources, including enumeration failures, and includes warnings naming the graph or project in the structured landing report while allowing other independent approval sources to qualify. Corrupt checkpoints remain visible through status and tick reporting, including after rebuilding the pending index. (#3400)
