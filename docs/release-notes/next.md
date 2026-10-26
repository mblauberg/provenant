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

- Fabric graph routes now accept optional `account` in `route` and every
  `routes` entry. Graph checkpoint envelopes under
  `fabric/graphs/<project>/graph-<id>.json` now require `execution.document`
  and `execution.directory`; their digest covers those host-only inputs.
  Existing checkpoints cannot be read or resumed by the new release. Finish
  active graphs with the previous release and archive their checkpoints
  before upgrading (#1712, #1713).

- Installer cut-over support is removed: the `--legacy-checkout` install flag,
  the `LEGACY_CHECKOUT` error code, pre-1.0 cleanup and Git launcher ownership
  are removed. `check install` no longer reports the finding rules
  `legacy_entry`, `legacy_hook_conflict`, `legacy_instruction_variable` or
  `legacy_checkout_unavailable`. Receipts with a non-null `legacy` object need
  a reinstall; see [receipt recovery](../runbooks/install.md#receipt-recovery).
  Current receipts with `"legacy":null` upgrade normally; newly written
  receipts omit the key (#1571).

## Behaviour changes

- Run settlement advances pending Fabric graphs in the same project after
  releasing custody. Dependants start automatically; `graph resume` remains
  manual recovery after a crash when no later run settles (#1712).

- `provenant output` on a terminal attempt that has no requested part now
  reports `NOT_FOUND` (exit 2). It used to report `NONTERMINAL` (exit 1)
  locally and `CONFLICT` over a peer. Over MCP the facade code is
  `not_found`. Active attempts still report `NONTERMINAL` (#1673).
