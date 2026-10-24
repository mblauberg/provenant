# Rust release and managed files

The `provenant-config` crate owns release stamping, packaging and managed-file
installation. See the accepted [state design](../specs/rust-port/design/state.md)
for the full contract. A staged release contains a verified manifest and
immutable assets; installation publishes its receipt last. Exact desired bytes
and links are no-ops, including after a crash before receipt publication.

## Build a staged release

From the Rust workspace, build the binary and run the build-only Cargo example.
Choose a fresh absolute output directory outside every repository checkout. The
output's parent directory must already exist. The example copies the exact asset
inventory digested by `build.rs`, reads the binary's `--version --json` identity,
and runs the install path's bounded version/help smoke before publishing
`release.json` last. It refuses existing output directories and asset drift.
Cargo does not install this example.

```sh
cd tools/front-door
export PATH="$HOME/.cargo/bin:$PATH"
export CARGO_TARGET_DIR="$(mktemp -d)/cargo-target"
RELEASE="$(mktemp -d)/release"
cargo build --release --locked --bin provenant
cargo run --locked --example package-release -- --out "$RELEASE"
```

Host builds use a fresh target directory. Confined writers use the separate
`$HOME/Library/Caches/provenant-lane-cargo/<sha256>` namespace, derived from
verified worktree and repository identities. Never use a lane cache for a host
build.

The binary defaults to `$CARGO_TARGET_DIR/release/provenant` (or the workspace's
`target/release/provenant` without that variable). Pass `--binary PATH` to package
another release build, for example one built with an explicit target triple.
Run `cargo build --release --locked --bin provenant` after source changes and
before packaging. The packager refuses a binary whose embedded source digest
does not match the current build's source digest.

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

Review every planned change and unresolved entry before applying it; a failed
preview prevents writes. Hooks default off. `--hooks on` registers hooks for
each selected harness that has a fixture-backed `HookSpec` (Claude today) and
refuses only when none does. To install into a further Claude config directory,
rerun `install --harness claude` with `CLAUDE_CONFIG_DIR` set; its MCP entry
then goes to that directory's `.claude.json`. Each directory is a separate
target in the one receipt, so runs accumulate and touch only the selected
directory. An exact legacy entry marked
`replace` is replaced only when that handler is available. Otherwise the
installer preserves it byte-for-byte and reports it as unresolved. `check
install` reports unresolved legacy entries as findings with exit 1 and compares
the installed binary, assets, receipt and owned selectors of every recorded
target, whatever the environment, without repair. To remove an installation,
run `provenant --state-dir "$STATE" uninstall --harness claude`; it removes only
unchanged owned entries and keeps the state and release assets. With
`CLAUDE_CONFIG_DIR` (or `CODEX_HOME`) set, uninstall removes only that
directory's target; without it, every recorded target of the harness. Reapply `install` after resolving a partial-write error.

`build.rs` stamps the source and asset digests; the packaging example produces
typed `release.json` with the binary and product assets. The CLI grammar is frozen in
[cli.md](../specs/rust-port/design/cli.md); `help --json` describes the command
surface without resolving roots or creating state.
