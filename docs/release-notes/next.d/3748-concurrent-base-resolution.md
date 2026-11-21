## Fixes

### Lanes and settlement

- Concurrent `provenant run implement --branch B --base origin/<branch>` launches in one repository now share a per-repository base-fetch lock, so their Git fetches no longer contend on the remote-tracking ref lock. A transient ref-lock collision is retried a bounded number of times and reported as launch contention rather than as an invalid base, so parallel launches resolve the same advertised commit that sequential launches do (#3748)
- A launch that fails while resolving its writer base now settles the graph node as failed in the persisted checkpoint at once. `provenant wait` and `provenant status` report the failure immediately instead of showing the launch as still running until the wait budget expires (#3748)
