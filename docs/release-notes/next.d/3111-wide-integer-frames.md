## Fixes
### Lanes and settlement

- A lane whose stream echoed an integer wider than 64 bits, such as a tool schema bound of `18446744073709552000`, no longer settles as `provider protocol is malformed` and loses its answer. The integer may appear in a frame the adapter otherwise ignores; the final result now decides the outcome for Claude, Codex, Cursor, Agy and OpenCode. A wide integer in a field Provenant reads, such as a usage count, still fails closed (#3111).
