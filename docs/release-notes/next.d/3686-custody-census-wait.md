## Fixes

### Lanes and settlement

- Custody regression checks wait for the guardian census record using its publication cadence and report the last reservation on timeout, reducing timing-related CI failures. (#3686)
