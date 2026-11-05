## Fixes
### Pipelines and graphs

- Refuse a writer continuation when its predecessor left uncommitted edits, and report the affected paths without discarding them (#2822).
