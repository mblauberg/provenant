# Rust release and managed files

The `provenant-config` crate owns release stamping, packaging and managed-file
installation. See the accepted [state design](../specs/rust-port/design/state.md)
for the full contract. A staged release contains a verified manifest and
immutable assets; installation publishes its receipt last. Exact desired bytes
and links are no-ops, including after a crash before receipt publication.

## Cut-over procedure

Use the absolute stable `provenant` binary and an explicit, fresh state root
outside every legacy root. Stage the release at an absolute path. Stop the
selected harness clients and config editors before preview, and keep them
stopped through installation and verification. The example selects Claude;
repeat `--harness` for each client, and provide `--legacy-checkout` only when
planning exact entries from that old checkout.

```sh
PROVENANT="$HOME/.local/bin/provenant"
"$PROVENANT" --state-dir "$STATE" install --source "$RELEASE" --harness claude --bin-dir "$HOME/.local/bin" --legacy-checkout "$LEGACY_CHECKOUT" --dry-run
"$PROVENANT" --state-dir "$STATE" install --source "$RELEASE" --harness claude --bin-dir "$HOME/.local/bin" --legacy-checkout "$LEGACY_CHECKOUT"
"$PROVENANT" --state-dir "$STATE" check install --source "$RELEASE"
```

Review every planned change before applying it; a failed preview prevents
writes. The default is `--hooks off`; enabling hooks requires a recorded
`HookSpec` policy and fixture for each registered handler. No hook policy has
landed yet, so `--hooks on` currently refuses before writing. Legacy Claude
`PostToolUse`/`Bash` groups also refuse cut-over until their host-specific
utility-hook interpreter has an exact recorded entry in `legacy.toml`; resolve
that registration explicitly instead of inferring ownership from its command.
`check install`
compares the installed binary, assets, receipt and owned selectors without
repair. To remove an installation, run
`provenant --state-dir "$STATE" uninstall --harness claude`; it removes only
unchanged owned entries and keeps the state and release assets. Reapply
`install` after resolving a partial-write error.

`build.rs` stamps the source digest, and the release build packages typed
`release.json` with the binary and product assets. The CLI grammar is frozen in
[cli.md](../specs/rust-port/design/cli.md); `help --json` describes the command
surface without resolving roots or creating state.
