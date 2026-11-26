## Fixes

### Build slots

- A warm slot target is no longer shared blindly across toolchains and
  worktrees. The new `build.target` setting (default `toolchain,worktree`) keys
  the Cargo target directory by the job's resolved `rustc -vV` identity and by
  the source checkout, so a warm cache built by an older toolchain cannot
  produce stale workspace-member artifacts in a pinned build and a worktree no
  longer reuses another worktree's members. `shared` keeps the legacy single
  target. The configured layer is reported by `check config` and `config show`,
  and `check config` warns when the toolchain recorded in a warm slot differs
  from the checkout's `rust-toolchain.toml` pin. (#4071)
