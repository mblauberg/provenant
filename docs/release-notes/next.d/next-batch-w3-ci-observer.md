## Changes

### Internal

- The landing gate and `wait --pr` read CI through one observer, whose retry
  and backoff values carry their source. (#3510)
