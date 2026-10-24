# Rust release and managed files

The `provenant-config` crate owns release stamping, packaging and managed-file
installation. See the accepted [state design](../specs/rust-port/design/state.md)
for the full contract. A staged release contains a verified manifest and
immutable assets; installation publishes its receipt last. Exact desired bytes
and links are no-ops, including after a crash before receipt publication.

## Install

Bootstrap from the binary in the staged release and use an explicit,
fresh state root outside every protected legacy root. Stage the release at an
absolute path. Quit Claude Code before installation because it rewrites
`~/.claude.json` while running. Close the other selected harness clients and
config editors before preview, and keep them closed through installation and
`check install`. The example selects Claude; repeat `--harness` for each
client, and set `--legacy-checkout` to the checkout whose exact entries are
being reconciled.

```sh
PROVENANT="$RELEASE/bin/provenant"
"$PROVENANT" --state-dir "$STATE" install --source "$RELEASE" --harness claude --bin-dir "$HOME/.local/bin" --legacy-checkout "$LEGACY_CHECKOUT" --dry-run
"$PROVENANT" --state-dir "$STATE" install --source "$RELEASE" --harness claude --bin-dir "$HOME/.local/bin" --legacy-checkout "$LEGACY_CHECKOUT"
"$PROVENANT" --state-dir "$STATE" check install --source "$RELEASE"
```

Review every planned change and unresolved entry before applying it; a
failed preview prevents writes. Hooks default off. `--hooks on` requires a
fixture-backed `HookSpec` for each handler. An exact legacy entry marked `replace` is replaced
only when that handler is available. Otherwise the installer preserves it
byte-for-byte and reports it as unresolved. `check install` reports unresolved
legacy entries as findings with exit 1 and compares the installed binary,
assets, receipt and owned selectors without repair. To remove an installation,
run `provenant --state-dir "$STATE" uninstall --harness claude`; it removes
only unchanged owned entries and keeps the state and release assets. Reapply
`install` after resolving a partial-write error.

`build.rs` stamps the source digest, and the release build packages typed
`release.json` with the binary and product assets. The CLI grammar is frozen in
[cli.md](../specs/rust-port/design/cli.md); `help --json` describes the command
surface without resolving roots or creating state.
