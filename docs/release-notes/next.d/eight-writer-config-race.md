## Fixes
### Git and safety

- Concurrent writer launches coordinate Git config snapshots with sibling updates, avoiding transient metadata refusals on Linux while keeping origin fetches concurrent.
