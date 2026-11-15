## Breaking changes
### Lanes and settlement

- **Update consumers of stop causes.** A blocked writer handoff settles with cause `blocked`; a refused launch records its specific cause instead of `preparation_refused`. (#3167)
