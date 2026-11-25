## Fixes

### Lanes and settlement

- GC coordinates collection with the settlement driver and retains a launched attempt's scratch until settlement completes, including between retries and when ownership cannot be determined, so a writer's private Git objects and a review source checkout are not collected mid-settlement and a correct commit is no longer reported as a failed import. Existing custody diagnostics remain visible. (#3975)
