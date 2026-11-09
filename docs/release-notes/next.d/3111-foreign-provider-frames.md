## Fixes
### Lanes and settlement

- A lane no longer settles as `provider protocol is malformed` and loses its answer because of content inside a provider frame that the adapter ignores. Provider frames are foreign content and are now decoded with plain JSON rules, bounded only by the 1 MiB line limit, for Claude, Codex, Cursor, Agy and OpenCode. This covers an integer wider than 64 bits (such as a tool schema bound of `18446744073709552000`), a repeated member, and nesting deeper than 64 levels; the final result decides the outcome. A value in a field Provenant reads, such as a usage count, that it cannot hold still fails closed (#3111).
