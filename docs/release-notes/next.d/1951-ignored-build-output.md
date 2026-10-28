## Fixes

- Ignored build output no longer blocks a writer dispatch. The macOS hard-link census skips directories Git ignores wholesale, such as an in-tree Cargo `target/` or `node_modules/`, and does not count their entries against the tree bound (#1951).
