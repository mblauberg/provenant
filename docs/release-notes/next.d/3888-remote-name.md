## Fixes

### Git and safety

- An empty or malformed Git remote name can no longer reach Git. A configured `git.remote` that is not a usable remote name is refused naming the value, `provenant gc` names the repair, and a stopped local-integration lane keeps its stop-publication remote. (#3888)
