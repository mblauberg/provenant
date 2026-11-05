## Fixes
### Pipelines and graphs

- Composite expansion errors identify invalid or duplicate node IDs and suggest how to correct them. Checkpoint limit errors also suggest splitting the graph into smaller batches (#2861).
- Large inherited disposition metadata is trimmed with a warning while finding identities and locations remain available.
