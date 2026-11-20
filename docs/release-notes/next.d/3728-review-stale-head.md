## Fixes

### Lanes and settlement

- `provenant run review --pr N` now accepts `--head SHA` to pin the expected PR head. When the forge still reports a different head just after a push, the host re-reads it within a bounded wait and refuses with both SHAs named if the mismatch persists, so a review can no longer silently run against the previous commit. Without `--head`, a local remote-tracking tip ahead of the forge head adds a warning naming both commits. The launch receipt reports the pinned base and head. (#3728)
