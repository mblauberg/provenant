## Fixes


### Lanes and settlement

- Seed builds clear inherited compiler-selection environment variables (`RUSTC_WRAPPER`, `RUSTC_WORKSPACE_WRAPPER`, `RUSTUP_*`) instead of refusing to run. (#3678)
- Lane Cargo debuginfo and incremental defaults now come from a single source. (#3669)
