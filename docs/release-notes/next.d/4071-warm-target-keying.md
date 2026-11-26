## Fixes

### Build slots

- A warm slot target is no longer shared blindly across toolchains and
  worktrees. The new `build.target` setting (default `toolchain,worktree`) keys
  the Cargo target directory by the compiler selected by Cargo, including a
  `+toolchain` argument. Its workspace-only rustc wrapper gives each checkout a
  separate workspace-member fingerprint while dependencies stay shared. A warm
  cache built by an older toolchain cannot produce stale workspace-member
  artifacts in a pinned build, and worktrees cannot reuse each other's members.
  `shared` keeps the legacy single
  target. The configured layer is reported by `check config` and `config show`,
  and `check config` warns when the toolchain recorded in a warm slot differs
  from the checkout's `rust-toolchain.toml` pin. (#4071)
