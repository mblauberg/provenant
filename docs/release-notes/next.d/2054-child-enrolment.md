## Fixes

- Nested runs now carry host-verifiable child run and attempt IDs. A lane that rewrites an ID is refused before launch and cannot affect a sibling's lookup or launch. Existing child records stay unindexed, with no migration (#2054).
