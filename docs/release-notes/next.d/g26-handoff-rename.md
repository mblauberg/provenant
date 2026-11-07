## Fixes
### Lanes and settlement

- Lanes are told to update their progress handoff by renaming a temporary file, because hard links are denied, and that a failed update never blocks the work.
