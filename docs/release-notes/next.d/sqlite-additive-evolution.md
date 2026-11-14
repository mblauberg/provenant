## Behaviour changes

### Fabric

- SQLite stores support additive schema changes within the same format major without a reset. Older and newer minor releases can share those stores; a reader that needs a missing schema addition asks for a writable open. (#3078)
