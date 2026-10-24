# Product and instance roots

The product checkout supplies code and shipped assets. The instance root owns
user instructions, configuration and machine-local state. The Rust state design
uses explicit roots and installs a verified, immutable release under the
selected fresh state root. It does not import an old receipt or discover an old
product checkout.

To relocate, choose an absolute state path outside every legacy root and stage
the packaged release at an absolute path. Use the stable binary, stop selected
harness clients and config editors, and preview the exact cut-over before
applying it. `--legacy-checkout` explicitly selects the one old checkout whose
compiled entries may be matched.

```sh
PROVENANT="$HOME/.local/bin/provenant"
"$PROVENANT" --state-dir "$STATE" install --source "$RELEASE" --harness claude --bin-dir "$HOME/.local/bin" --legacy-checkout "$LEGACY_CHECKOUT" --dry-run
"$PROVENANT" --state-dir "$STATE" install --source "$RELEASE" --harness claude --bin-dir "$HOME/.local/bin" --legacy-checkout "$LEGACY_CHECKOUT"
"$PROVENANT" --state-dir "$STATE" check install --source "$RELEASE"
```

Keep the harnesses and config editors stopped through `check install`. The
installer prints each matched change before writing and rejects unmatched
foreign entries. `uninstall --harness NAME` removes only unchanged entries in
the receipt; it never recursively deletes a checkout, state root or release.
