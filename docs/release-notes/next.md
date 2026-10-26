# Next release

Every install's source digest changes once with this release. `check install`
reports existing installs as stale until they are reinstalled.

The source digest now covers shipped workspace source (each member's `src/`,
`assets/` and `build.rs`), manifests, the lockfile, the toolchain and Cargo
configuration, the release packager (`examples/package-release.rs`) and the
bundled assets. Edits under a member's `tests/`, `benches/` and other `examples/`
no longer change it.

## Stability

The v1.0.0 stability promise is withdrawn. Commands, flags, `--json` output,
`config.toml` keys, the state layout and Fabric MCP tool names and schemas can
change in any release, with no deprecation period or compatibility shims. Each
release's notes list its breaking changes. Upgrade every host together, since
peers on different versions are not supported.

## Breaking changes

None yet.

## Behaviour changes

- `provenant output` on a terminal attempt that has no requested part now
  reports `NOT_FOUND` (exit 2). It used to report `NONTERMINAL` (exit 1)
  locally and `CONFLICT` over a peer. Over MCP the facade code is
  `not_found`. Active attempts still report `NONTERMINAL` (#1673).
