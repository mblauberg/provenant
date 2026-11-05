## Breaking changes
### Lanes and settlement

- **Lane JSON uses one projection.** Update consumers of `status`, `output`, `cancel` and `land` to read the shared identity, state and retry-lineage fields and structured paths (#2353).
