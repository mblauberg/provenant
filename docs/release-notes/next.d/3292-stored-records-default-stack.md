## Fixes

### Fabric

- Stored records deserialise on the default debug thread stack: nested `#[serde(flatten)]` chains in `records` and `fabric-runtime` are replaced with single-pass readers that keep unknown fields for round trips. The `RUST_MIN_STACK` override and the 8 MiB `check_providers --live` worker stacks are removed; stored JSON is unchanged. (#3292)
