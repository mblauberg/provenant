## Additions

### Git and safety

- `scripts/prepare-release vX.Y.Z` bumps every workspace member's package version, refreshes the lock and the bug-template placeholder, collates the notes and warns about merged changes without a next.d fragment. `provenant check release` now names the missing tracked path in its evidence error. (#3527)
