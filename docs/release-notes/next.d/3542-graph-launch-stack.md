## Fixes

### Fabric and graphs

- A graph launch now fits the default 2 MiB stack of a spawned thread in a debug build. The large scheduling pass in `advance_inner` runs in its own frame, and large values on the submit path are boxed. The `RUST_MIN_STACK` override and the 8 MiB `check providers --live` worker stack are removed. (#3542)
