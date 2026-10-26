## Breaking changes

- Move OS operations from `provenant-supervisor` into `provenant-os`, below
  process supervision. Callers import the new crate directly.
- Replace the injectable filesystem traits with concrete `Directory` and
  `FileHandle` operations. Stable bounded reads and publication durability
  checks share one implementation. On-disk formats and command behaviour
  are unchanged.
