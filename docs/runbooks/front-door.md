# Rust release and managed files

The `provenant-config` crate owns source stamping and managed-file installation
for the Rust port. See the accepted [state design](../specs/rust-port/design/state.md)
for the full contract.

`build.rs` stamps the workspace source digest. Post-link build orchestration
calls `package_release` to seal staged files with typed `release.json`. Installation verifies the manifest, file bytes and modes, then puts
the release at `<state>/releases/<SourceDigest>/`. It applies managed files
under `<state>/install/install.lock` and publishes `<state>/install/receipt.json` last.
Exact desired bytes and links are no-ops, including after a crash before receipt
publication. A dry run creates no root, lock or receipt; `check install` compares
digests without repair; uninstall removes only unchanged owned entries.

The Rust crate exposes these operations as library entrypoints. The CLI does not
yet wire `install`, `check install` or `uninstall`, and the legacy
`scripts/install-harness`, `scripts/install-skills` and
`scripts/check-provenant-install.py` entrypoints refuse the retired Python path.
The library requires explicit harness selection and manages per-entry links.
Client MCP/hook editors and legacy-entry matching are part of the later harness cut-over. No command-line installation procedure is
available in this slice.
