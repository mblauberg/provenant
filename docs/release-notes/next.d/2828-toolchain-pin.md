## Breaking changes
### Worktrees and lanes

- Lane build seeding now requires `tools/front-door/rust-toolchain.toml`; move a fixed release pin from the old `rust-toolchain` file into its `[toolchain]` table (#2828).
