## Fixes

### Fabric

- Cross-host launches with a review base dispatch on the peer again: the peer rebuilds the graph document with the same schema version the sender derives from its fields, so the document no longer fails admission with `graph document schema_version is invalid`. (#3752)
