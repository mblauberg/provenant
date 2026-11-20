## Additions

### Builds and caches

- Build commands are configured per repository, with no files in the repository's tree. A host-project config layer declares build ports and test tiers, and pure stack detection fills in sensible defaults, such as a pnpm root with nested Rust. A malformed tier command warns and falls back to the next lower layer, naming where each value came from. (#3654)
- `build.prepare` takes a list of argv arrays and runs from the checkout root before seed builds. Seeds that are failed or stale now warn at lane start and in `provenant worktree doctor`; `build.seed.stale_after` sets the staleness threshold. (#3676)
- Affected selection reads `cargo metadata` JSON and limits test tiers to the changed crates and the crates that depend on them. It falls back to the full suite for broad changes and periodically. (#3668)
- Seed builds hold dependency units only, build check-mode units and the lint target independently, and identify a seed by its effective dependency inputs: the lockfile, the dependency-deciding parts of each manifest, the profile tables, the effective build profile and the toolchain. A seed can be rebuilt more often than strictly needed but is never reused when stale. Refresh requests carry the lane's commit. (#3671, #3672, #3673)

## Fixes

### Builds and caches

- Seed builds clear inherited compiler-selection environment variables (`RUSTC_WRAPPER`, `RUSTC_WORKSPACE_WRAPPER`, `RUSTUP_*`) instead of refusing to run. (#3678)
- Lane Cargo debuginfo and incremental defaults now come from a single source. (#3669)

## Upgrade notes

- `config/ports.json` tier commands are now objects. Binaries older than this release warn "malformed port layer ignored" for a repository that uses the new shape, and fall back to their built-in tiers until the host is upgraded.
