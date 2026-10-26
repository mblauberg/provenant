# Next release

Every install's source digest changes once with this release. `check install`
reports existing installs as stale until they are reinstalled.

The source digest now covers shipped workspace source (each member's `src/`,
`assets/` and `build.rs`), manifests, the lockfile, the toolchain and Cargo
configuration, the release packager (`examples/package-release.rs`) and the
bundled assets. Edits under a member's `tests/`, `benches/` and other `examples/`
no longer change it.

## Breaking changes

- Installer cut-over support is removed: `install --legacy-checkout` is no
  longer accepted, receipts with legacy fields fail with a reinstall error,
  and pre-1.0 cleanup, legacy `check install` findings and Git launcher
  ownership are removed. Move the old receipt aside and reinstall; see
  [receipt recovery](../runbooks/install.md#receipt-recovery) (#1571).

## Behaviour changes

- `provenant output` on a terminal attempt that has no requested part now
  reports `NOT_FOUND` (exit 2). It used to report `NONTERMINAL` (exit 1)
  locally and `CONFLICT` over a peer. Over MCP the facade code is
  `not_found`. Active attempts still report `NONTERMINAL` (#1673).
