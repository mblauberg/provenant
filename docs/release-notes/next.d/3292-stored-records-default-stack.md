## Fixes

### Fabric

- Stored records deserialise on the default debug thread stack: nested `#[serde(flatten)]` chains in `records` and `fabric-runtime` are replaced with single-pass readers that keep unknown fields for round trips. Stored JSON is unchanged. The `RUST_MIN_STACK` override and the 8 MiB `check_providers --live` worker stacks stay for now: debug-build graph launches still need more than 2 MiB (#3542). (#3292)
