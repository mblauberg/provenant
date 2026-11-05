## Fixes
### Lanes and settlement

- A writer continuation after predecessor commits now launches pinned to the predecessor head instead of being refused (#2786).
- Uncommitted predecessor edits are preserved during writer continuation and listed in the continuation brief, or named in continuation refusals (#2786).
- Graph execution follows quota-limited writer continuations to terminal success and preserves the node source as the lens review diff base (#2786).
