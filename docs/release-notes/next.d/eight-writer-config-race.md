## Fixes
### Git and safety

- Concurrent writer launches coordinate Git config snapshots with sibling updates, avoiding transient metadata refusals on Linux while keeping origin fetches concurrent.

- Git config normalisation reports execution, timeout and output failures separately from unsafe-directory metadata.

- Repeated verification of an unchanged config snapshot reuses its successful canonical parse, reducing competing Git subprocess launches.
