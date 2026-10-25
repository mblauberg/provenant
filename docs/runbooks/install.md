# Build, package and install

The `provenant-config` crate owns release stamping, packaging and managed-file
installation. See the [state design](../specs/state.md) for
the full contract. A staged release contains a verified manifest and immutable
assets. Installation publishes its receipt last, and exact desired bytes and
links are no-ops, including after a crash before receipt publication.

## Build a staged release

From the Rust workspace, build the binary and run the build-only Cargo example.
Choose a fresh absolute output directory outside every repository checkout. The
output's parent directory must already exist. The example copies the exact asset
inventory digested by `build.rs`, reads the binary's `--version --json` identity,
and runs the install path's bounded version and help smoke before publishing
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
verified worktree and repository identities. It also holds the lane's private
Cargo home, npm and pnpm caches and Corepack home; the host's `~/.cargo/registry`,
`~/.npm`, `~/Library/pnpm` and similar stores are read-only to lanes. Never use
a lane cache for a host build. `provenant worktree remove` deletes only the
removed worktree's own cache while holding its writer lock. Other orphaned
caches remain until `provenant gc [--dry-run] [--idle-days N]`, which also
removes caches idle for N days (default 7) for the selected project's
worktrees, sparing live or unobservable writers. V1 owner markers support
orphan cleanup and foreign-owner reporting, but never idle cleanup because
they lack device and inode identity. Existing owners outside that project are
reported as kept (foreign). Before deletion, GC atomically renames
each cache to a tombstone so a launching lane can create a fresh cache. Later
sweeps retry interrupted tombstones. Entries that fail identity verification
are quarantined under `.gc-refused-*` and kept for inspection. The owner marker
is deleted last, and a failed sweep reports completed removals and bytes
already freed alongside the error.

The packager defaults to `$CARGO_TARGET_DIR/release/provenant`, or the
workspace's `target/release/provenant` without that variable. Pass
`--binary PATH` to package another release build, for example one built with an
explicit target triple. Run `cargo build --release --locked --bin provenant`
after source changes and before packaging. The packager refuses a binary whose
embedded source digest does not match the current build's source digest.

## Install

Bootstrap from the binary in the staged release, with an explicit state root
at an absolute path. The default root is `~/.local/share/provenant/v1`. Quit
Claude Code before installing, because it rewrites `~/.claude.json` while
running. Close the other selected harness clients and config editors before the
preview, and keep them closed through the install and `check install`. The
example selects Claude; repeat `--harness` for each client (`claude`, `codex`,
`cursor`, `agy` or `opencode`).

Claude accounts must use exactly `~/.claude-$LABEL`, where `LABEL` is the
registration label. Choose a label and sign in to that directory before
registering it. `~/.claude` is the sign-in source only, not a registered account.

```sh
PROVENANT="$RELEASE/bin/provenant"
STATE="$HOME/.local/share/provenant/v1"
"$PROVENANT" --state-dir "$STATE" install --source "$RELEASE" --harness claude --bin-dir "$HOME/.local/bin" --dry-run
"$PROVENANT" --state-dir "$STATE" install --source "$RELEASE" --harness claude --bin-dir "$HOME/.local/bin"
"$PROVENANT" --state-dir "$STATE" check install --source "$RELEASE"
LABEL=work
ACCOUNT_DIR="$HOME/.claude-$LABEL"
CLAUDE_CONFIG_DIR=~/.claude-$LABEL claude
"$PROVENANT" --state-dir "$STATE" accounts register "$LABEL" --adapter claude --directory "$ACCOUNT_DIR"
"$PROVENANT" --state-dir "$STATE" quota tick
```

Repeat the account registration for each account, with a unique label and
`--adapter codex` for Codex accounts. Dispatch needs registered accounts; run
`quota tick` afterwards to initialise quota state.

Review every planned change before applying it; a failed preview prevents
writes. `--dry-run` creates no state root, lock or receipt. A first install
leaves hooks disabled unless requested. Re-install without `--hooks` keeps
ownership of previously installed hooks, including the legacy Git pre-push
launcher, and refreshes unchanged entries for the new installation. Edited
hooks and launchers stay in place, leave the receipt and produce findings.
Missing launchers also leave the receipt. The legacy checkout flag need not
be repeated. `--hooks off` removes unchanged owned hooks and relinquishes
modified ones, reporting them. Explicit `--hooks on` refuses an edited Claude
hook; use `--hooks off` to relinquish its ownership before reinstalling.
The receipt records one legacy checkout. Remove or relinquish its pre-push
launcher with `--hooks off` before migrating another checkout.
`--hooks on` registers hooks for each selected harness that has a
fixture-backed hook specification (Claude today). At least one selected
harness must have a specification. Unmanaged paths are preserved, and
conflicts are refused unless
`--adopt-existing` is given for files that match exactly.

To install into a further Claude config directory, rerun
`install --harness claude` with `CLAUDE_CONFIG_DIR` set; its MCP entry then goes
to that directory's `.claude.json`. Each directory is a separate target in the
one receipt, so runs accumulate and touch only the selected directory. The same
applies to Codex with `CODEX_HOME`.

`check install` compares the installed binary, assets, receipt and owned
selectors of every recorded target, without repair. Reapply `install` after
resolving a partial-write error.

On macOS, an install into this account's default state root also manages
`~/Library/LaunchAgents/dev.provenant.peer.plist`. The dry-run and install
report name its action, and the receipt records its exact contents and mode.
Symlinked plist paths or parents are refused. The agent's PATH contains the
fixed system directories, the install bin directory and existing absolute
directories holding resolved Claude, Codex and Git executables, when known.
It does not inherit the shell's other PATH entries.
An exact plist from the earlier installer is recognised for upgrade or
uninstall using its recorded binary path and state root. Modified or unknown
plists are preserved and block install. To recover, move the named plist
aside, run `launchctl bootout gui/$UID/dev.provenant.peer`, then rerun install.

To remove an installation, run
`provenant --state-dir "$STATE" uninstall --harness claude`. It removes only
unchanged owned entries and keeps the state and release assets. It never
recursively deletes a checkout, state root or release. With `CLAUDE_CONFIG_DIR`
or `CODEX_HOME` set, uninstall removes only that directory's target; without
it, every recorded target of the harness.
Removing the last recorded target also boots out the peer LaunchAgent and
removes its unchanged plist. A modified plist is kept and reported; its
service is left running. A failed bootout keeps the plist for a retry.
A missing peer plist is stopped by its service label.

`build.rs` stamps the source and asset digests, and the packaging example
produces the typed `release.json`. The CLI grammar is described in
[cli.md](../specs/cli.md); `help --json` describes the command
surface without resolving roots or creating state.

## Migrating from a pre-1.0 install

Only an install that predates the Rust binary needs this. Pass
`--legacy-checkout PATH` to name the one old checkout whose compiled entries
the installer may match, and choose a fresh state root outside every legacy
root. The installer does not discover an old checkout or import an old receipt.

```sh
LEGACY_CHECKOUT=/absolute/path/to/old/checkout
"$PROVENANT" --state-dir "$STATE" install --source "$RELEASE" --harness claude --bin-dir "$HOME/.local/bin" --legacy-checkout "$LEGACY_CHECKOUT" --dry-run
```

The preview prints each matched change and each unresolved entry. An exact
legacy entry marked `replace` is replaced only when that handler is available.
Otherwise the installer preserves it byte for byte and reports it as
unresolved, and `check install` reports unresolved entries as findings with
exit 1. Apply the same command without `--dry-run` once the preview is right.

## Fabric MCP registration

`provenant fabric mcp [--client NAME]` is the explicit stdio transport. The
`--client` flag is optional; supported values are `claude`, `codex`, `cursor`,
`agy` and `opencode`. `--json`
is refused before stdin. A bare `provenant` prints usage and exits 2, so a
client registration must name `fabric mcp`.

Installation registers the transport for each selected harness and preserves
unrelated client configuration; there is no separate registry editor. See the
[Fabric design](../specs/fabric.md).
