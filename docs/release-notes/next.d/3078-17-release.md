## Breaking changes
### Fabric

- **Upgrade peer hosts together.** Peer replies must declare their protocol, and a missing or invalid reply reports uncertainty without retrying a mutation. (#3078)
