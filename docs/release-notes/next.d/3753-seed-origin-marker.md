## Fixes

### Lanes and settlement

- Seed-clone `build.prepare` commands now receive `PROVENANT_SEED_ORIGIN`, the absolute path of the origin checkout, as an explicit environment marker; the verify-base Cargo path that sets `CARGO_TARGET_DIR` does not. Consumers can tell a standalone seed clone from an ambient target directory and share tool caches with the origin checkout. (#3753)
