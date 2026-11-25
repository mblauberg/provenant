## Fixes

### Lanes and settlement

- GC leaves an attempt's scratch alone while a settlement driver owns its import and final reconciliation, or its ownership cannot be determined, so a writer's private Git objects and a review source checkout are not collected mid-settlement and a correct commit is no longer reported as a failed import. (#3975)
