## Fixes

### CLI and install

- `provenant output --full GRAPH/NODE#N --host` now prints the peer's real committed head and publication from its lane projection instead of omitting them; read-only and unpublished lanes still omit non-applicable headers. (#3807)
