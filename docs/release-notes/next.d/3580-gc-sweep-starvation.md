## Fixes

### Lanes and settlement

- `provenant gc --dry-run --full` no longer reports `0 Cargo target(s)` with an empty budget setting and source on a host with large caches. A 4096-entry cap on each cache measurement ended the sweep at the first large lane cache, so the target seed and Cargo target classes never ran and printed empty reports as measured zeros. Measurement now stops only at the sweep deadline. A class that a pass does not reach reports `not run in this pass`. (#3580)
- GC no longer keeps a Cargo target for ever because one of its top-level directories holds more than 1000 entries. The Cargo lock probe gave up at that count and reported a failed probe on every pass. It is now bounded only by its depth and the sweep deadline. (#3580)
