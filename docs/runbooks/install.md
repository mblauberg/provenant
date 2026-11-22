# Install, upgrade, verify and remove
Owner: provenant-config
Anchor: [`managed.rs`](../../tools/front-door/crates/config/src/managed.rs); [`shared_settings.rs`](../../tools/front-door/crates/config/src/shared_settings.rs)

Invariants are in the [state spec](../specs/state.md); flags are in
`provenant help --all`. Install writes its receipt last, so rerunning `install`
recovers any partial or crashed install.

## Bootstrap and upgrade

`./install.sh` installs the release for the checkout's exact tag, else the latest.
`--tag vX.Y.Z` selects one; `--source [DIR]` builds a checkout with its pinned
toolchain (`--yes` accepts a missing-toolchain install). Releases are
`aarch64-apple-darwin` only; other hosts use `--source`. The script verifies the
package SHA-256 before running anything from it. State defaults to
`~/.local/share/provenant/v2` and the binary to `~/.local/bin`; override with
`--state-dir`, `PROVENANT_STATE_DIR` and `--bin-dir` (absolute paths).

Agent installation finds `rustc` on the installing shell's PATH.
`provenant check install` warns if an installed agent PATH would stop a trusted
rustup toolchain being selected; reinstall from a shell whose PATH finds the
rustup shim.

A first install on a terminal prompts for providers, instruction files (replace,
append or skip), native harness hooks and Claude accounts. `--yes` and any rerun
with a receipt skip the prompts. A bare `provenant install --source "$RELEASE"`
targets every detected client CLI and `~/.claude-*` directory; with a receipt it
reuses the recorded targets and binary directory.

### First 2.0 install after 1.x

Wind down every lane first. The installer validates the release and staged
install before stopping agents, then installs into the fresh v2 root. It copies
only `<state>/config.toml` and `host-id` from the default 1.x root and never writes that
root, so reinstalling 1.x rolls back; run state and the old receipt are not
migrated, and work started under 2.x stays in the v2 root (collect it with
`provenant output` before rolling back). Install replaces or removes its declared
Provenant artifact names, including 1.x Fabric MCP registrations and
LaunchAgents. Select harnesses again by autodetection or `--harness`, then inspect
the 1.x root and client configuration yourself: leftovers outside the declared
names are yours to remove. A custom 1.x root is not copied; use a fresh custom root
and configure it by hand.

### Upgrade

```sh
provenant upgrade
provenant upgrade --from-source /absolute/path/to/provenant
```

Upgrade needs a receipt. It upgrades this host and every registered Fabric peer
one after another over Fabric's SSH settings, and preflights every host before
installing any. Source upgrades need build tools on every host and a stable
checkout. Keep the fleet quiescent: a failure on one host leaves hosts on
different versions, and there is no distributed rollback. Fix the reported cause
and rerun.

Lanes need not drain. Install swaps the `provenant` link atomically and keeps
every release directory a running process still uses, so a live supervisor and its
provider keep their binary. Two things cross the swap: later re-executions through
the link (settlement resume, tick, the peer agent) run the new release against the
live attempt's records, and install restarts the peer and tick agents. When the
candidate's live-attempt record format differs from the installed one, install
warns on stderr and names every live attempt under the installed state root (or
warns that it could not inspect them). It never refuses on this warning. Drain
first, or expect those attempts to settle under the new release; drain too when a
multi-host upgrade would leave peers on mixed versions for long.

If installation fails after stopping agents, the output names the stopped agents
and those that may still be running, prints the quoted state directory and asks
you to rerun the original install command with its original options (`--source`,
`--state-dir`, `--bin-dir`). Agents restart after a successful install.

`provenant upgrade --verify` then runs the bundled smoke graph (`--host PEER`
places it on a peer; `--route` and repeatable `--set NAME=VALUE` override its
routes and parameters), and adding `--dry-run` previews it without upgrading. Its
write node runs in a worktree the verification creates on a branch of a scratch
fixture. `provenant run smoke` by hand, from any checkout, runs only the read-only
nodes (the receipt names the skipped write node); add `--branch B --base REF` or
`--worktree PATH` to run the write node too.

### Bundled macOS app

Release packages carry a separate signed asset,
`provenant-apps-aarch64-apple-darwin.tar.gz`, built by the release workflow with
`apps/quota-menubar/bundle`. The shell installer downloads and verifies its
checksum, installs `Provenant Console.app` into `~/Applications`, migrates the
`dev.provenant.quota-menubar` LaunchAgent to the app's nested menu helper and
restarts it when it was running. `provenant upgrade` and `provenant install`
report the installed app's `CFBundleShortVersionString`; when it trails the CLI
they say `app is behind CLI`, and `provenant hosts doctor` repeats that advisory.
A source build (`--source`) has no signed app asset, so install and upgrade warn
and leave the existing app in place rather than refusing the CLI installation.
A missing asset, bad checksum or damaged archive is likewise a warning: the CLI
is still installed and the previous app is left untouched.

Set `PROVENANT_APPS_DIR` to an absolute directory to keep the app somewhere
other than `~/Applications`; install, upgrade, doctor and the installer all
honour it. LaunchServices opens the console by bundle identifier regardless of
the directory.

### Test a candidate binary

When a release changes a persisted schema, add its previous shape to the
[upgrade corpus](../../tools/front-door/tests/upgrade-corpus/v2.4.1/README.md) and
exercise it in the macOS release smoke.

CI's macOS release smoke keeps a checked package in the
`candidate-release-<workflow SHA>` artifact for seven days. Verify its checksum,
commit, tree and platform metadata against the intended candidate before
extracting it. The smoke build disables LTO; the published release is the install
target.

Run an uninstalled candidate only with its own `--state-dir` and `PROVENANT_ROOT`
set to its checkout. A candidate can change persisted state, so never point it at
an installed binary's state. `install --source DIR --upgrade-preflight` is
read-only and safe on real state.

## Build a staged release

Only for `--source` installs and candidate releases. Build outside a Fabric lane
into a fresh absolute output directory outside every checkout (its parent must
exist), and never use a lane cache for a host build.

```sh
cd tools/front-door
export CARGO_TARGET_DIR="$(mktemp -d)/cargo-target"
export RUSTC_WRAPPER=.cargo/rustc-wrapper.sh    # reproducible path remapping
RELEASE="$(mktemp -d)/release"
cargo build --release --locked --bin provenant
cargo run --locked --bin package-release -- --out "$RELEASE"
unset CARGO_TARGET_DIR RUSTC_WRAPPER
```

The packager refuses an existing output directory, asset drift, or a binary whose
embedded source digest differs from the current source, so rebuild after any
source change. `--binary PATH` packages another build.

## Install

Read the preview for the native configuration files it will write, and keep
clients and editors that rewrite those files closed from the preview through
`check install`: the installer checks for drift before each write, but that is not
an atomic compare-and-swap. In particular quit Claude Code when the plan writes
`~/.claude.json`. Managed instruction or skill links alone need no client closed.
Claude accounts live in exactly `~/.claude-$LABEL`; sign in there before
registering. `~/.claude` is a sign-in source, never an account.

```sh
PROVENANT="$RELEASE/bin/provenant"
STATE="$HOME/.local/share/provenant/v2"
"$PROVENANT" --state-dir "$STATE" install --source "$RELEASE" --harness claude --dry-run
"$PROVENANT" --state-dir "$STATE" install --source "$RELEASE" --harness claude
"$PROVENANT" --state-dir "$STATE" check install --source "$RELEASE"
LABEL=work
CLAUDE_CONFIG_DIR=~/.claude-$LABEL claude    # sign in, then exit
"$PROVENANT" --state-dir "$STATE" accounts register "$LABEL" --adapter claude --directory "$HOME/.claude-$LABEL"
"$PROVENANT" --state-dir "$STATE" quota tick
```

Repeat `--harness` per client (`claude`, `codex`, `cursor`, `agy`, `opencode`) and
`accounts register` per account (`--adapter codex` for Codex). Dispatch needs
registered accounts, and `quota tick` initialises quota state.

- A failed preview prevents writes.
- Hooks stay off on a first install. `--hooks on` registers each selected
  harness's declared native events ([hook behaviour](claude-hooks.md)). Omitting
  `--hooks` keeps owned hooks and refreshes unchanged ones; `--hooks off` removes
  unchanged ones and releases edited ones. An edited managed hook is preserved:
  follow the reported recovery before replacing it.
- Supported global instruction files link the release's instruction asset.
  Findings name the reason and source for any unavailable native integration.
  Provenant registers no MCP server; agents use the CLI.
- Occupied or foreign entries are refused. If the preview shows an exact match you
  accept, rerun with `--adopt-existing`.
- A directory that is not a registered account needs `install --harness claude`
  with `CLAUDE_CONFIG_DIR` set (`CODEX_HOME` for Codex); each directory is a
  separate target in the one receipt.
- `check install` compares binary, assets, receipt and owned entries for every
  target without repairing.

## Account sync

Every registered account directory holds the same managed skill links, managed
hooks and allow-listed shared Claude settings. `install`, `accounts register` and
`accounts sync` apply them; credentials, identity fields and user-owned entries
are never synced. An edited managed hook stays in place and is reported as
`relinquished_modified_entry`; an account with no managed hooks has relinquished
them until `--hooks on`.

Set the shared keys under `[domains.accounts.shared_settings]`. When
`check install` reports a settings conflict, `provenant accounts sync --adopt
ACCOUNT` makes that account the source and `--force-settings` replaces its values
from the source. For other account drift, follow the reported repair: preview with
`accounts sync --dry-run`, resolve unmanaged conflicts, then run
`provenant accounts sync`. After a partial write, rerun the command.

## Background agents

On macOS, the default-state install manages the peer and tick LaunchAgents; check
them with `provenant hosts doctor`. If a modified plist blocks install, move it
aside, boot out the agent with `launchctl` and rerun install. Peer setup is in
[Fabric hosts](fabric-hosts.md#macos-peers-and-the-login-keychain) and scheduled
collection in [GC](gc.md).

On Linux, the default-state install enables a user timer. To run it while logged
out, run `loginctl enable-linger $USER`.

## Uninstall

```sh
provenant --state-dir "$STATE" uninstall --harness claude
```

This removes only entries that still match the receipt, never recursively, and
keeps state and release assets. With `CLAUDE_CONFIG_DIR` or `CODEX_HOME` set it
removes that directory's target only. Removing the last target also stops the
LaunchAgents or timer; a modified plist or unit is kept and reported, and a failed
bootout keeps the plist for a retry.

If the home or state root moved since install, uninstall reports
`relocated_stale_entry` (and `state_root_moved`) and never touches the old paths.
Finish by hand: `launchctl bootout gui/$UID/dev.provenant.peer`, move the old peer
plist aside, delete links under `~/.local/bin`, the harness skills directories and
instruction links that point into the old state root, then rerun
`install --adopt-existing`.

## Receipt recovery

A receipt that fails to decode reports a reinstall error. Move
`<state>/install/receipt.json` aside, preview, and reapply `install`; exact existing
files and links are no-ops. Links into another release conflict: remove those
binary, skill and instruction links first. Use `--adopt-existing` for a regular
executable that must be replaced.
