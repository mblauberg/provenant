## Fixes

### Lanes and settlement

- Released read-only scratch expires after `gc.idle` while results, handoffs and logs remain durable. GC uses an overridable sweep budget, resumes interrupted work and warns about stale sweep holders and unrelated EPERM processes. (#3518)
