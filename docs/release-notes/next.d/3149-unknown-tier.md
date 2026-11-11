## Fixes
### Pipelines and graphs

- A review child launch that names an unknown `tier` is now refused under the default advisory diversity as well as under `strict`, with one usage error that names the tier and lists the valid tiers, instead of a graph compile error (#3149).
