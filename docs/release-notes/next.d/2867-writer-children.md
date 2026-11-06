## Fixes
### Lanes and settlement

- Writer children integrate into their parent branch while preserving the parent's staged and unstaged edits. Child waits yield the parent's graph slot and clear it when the wait returns (#2867).
