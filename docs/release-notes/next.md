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

- Installer cut-over support is removed: the `--legacy-checkout` install flag,
  the `LEGACY_CHECKOUT` error code, pre-1.0 cleanup and Git launcher ownership
  are removed. `check install` no longer reports the finding rules
  `legacy_entry`, `legacy_hook_conflict`, `legacy_instruction_variable` or
  `legacy_checkout_unavailable`. A receipt's `legacy` record is now ignored and
  dropped on the next install write, so receipts from the cut-over installer
  upgrade normally. Any Git hook launchers that installer placed under the
  recorded `git_hooks_dir` are no longer managed; remove them by hand if wanted
  (#1571).

## Behaviour changes

- `provenant output` on a terminal attempt that has no requested part now
  reports `NOT_FOUND` (exit 2). It used to report `NONTERMINAL` (exit 1)
  locally and `CONFLICT` over a peer. Over MCP the facade code is
  `not_found`. Active attempts still report `NONTERMINAL` (#1673).
