# Observation and repository ports

## Fixes

### Lanes and settlement

- Lane-cap reference text derives supported behaviour from the executable policy table. Live steering help and the fabric spec now disclose that every provider returns `control_unsupported`; delivery transports follow in 2.1 (#3278). (#3078)

### CLI and install

- Reference generation includes the declared outward interfaces and their JSON schema owners. Internal state-file readers remain unsupported. (#3078)

### Pipelines and graphs

- Repository tier checks read the single declaration in `config/ports.json`; `.provenant/ports.json` is an optional link to it. CI no longer depends on the agent-integration directory. (#3078)
