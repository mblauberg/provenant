## Fixes
### Lanes and settlement

- A writer continuation after predecessor commits now launches pinned to the predecessor head instead of being refused (#2786).
- Uncommitted predecessor edits are preserved during writer continuation and listed in the continuation brief, or named in continuation refusals (#2786).
