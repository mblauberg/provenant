## Fixes

### Git and safety

- Delivery uses one typed destination and the same remote-name grammar as the forge resolver, so names such as `team/upstream` are accepted consistently. Local integrations record a local destination; direct decoding refuses an empty remote name. (#3895)
