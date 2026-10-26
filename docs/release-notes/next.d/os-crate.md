## Breaking changes

- Move OS operations from `provenant-supervisor` into `provenant-os`, below
  process supervision. Callers import the new crate directly.
- Replace the injectable filesystem traits with concrete `Directory` and
  `FileHandle` operations. Stable bounded reads and publication durability
  checks share one implementation. On-disk formats and command behaviour
  are unchanged.
- Stable file reads expose typed read-I/O errors. Callers share
  `Metadata::unchanged` when comparing read-relevant metadata.
