## Fixes

### Lanes and settlement

- Reclaim idle private lane Cargo caches during full and low-disk sweeps while protecting active owners and held locks. GC previews use bounded cache measurements. (#3488)
