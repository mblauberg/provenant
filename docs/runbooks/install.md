# Install, upgrade, verify and remove
Owner: provenant-config
Anchor: [`managed.rs`](../../tools/front-door/crates/config/src/managed.rs); [`shared_settings.rs`](../../tools/front-door/crates/config/src/shared_settings.rs)

Invariants are in the [state spec](../specs/state.md). Flags are in
`provenant help --all`. Install writes its receipt last; rerunning `install`
recovers any partial or crashed install.

## Bootstrap and upgrade

`./install.sh` installs the release for the checkout's exact tag, else the latest.
`--tag vX.Y.Z` selects one; `--source [DIR]` builds a checkout with its pinned
toolchain (`--yes` accepts a missing-toolchain install). Releases are
`aarch64-apple-darwin` only; other hosts must use `--source`. The script
verifies the package SHA-256 before running anything from it. State defaults to
`~/.local/share/provenant/v2` and the binary to `~/.local/bin`; override with
`--state-dir`, `PROVENANT_STATE_DIR` and `--bin-dir` (absolute paths).

A first install on a terminal prompts for providers, instruction files (replace,
append or skip), Claude hooks and Claude accounts. `--yes` and any rerun with a
receipt skip the prompts. A bare `provenant install --source "$RELEASE"` targets
every detected client CLI and `~/.claude-*` directory; with a receipt it reuses
the recorded targets and binary directory.

### Upgrade

```sh
provenant upgrade
provenant upgrade --from-source /absolute/path/to/provenant
```

Upgrade needs a receipt. It upgrades this host and every registered Fabric
peer one after another, over Fabric's SSH settings. Source upgrades need build
tools on every host and a stable checkout. A failure on one host leaves hosts on
different versions: fix the reported cause and rerun; there is no distributed
rollback.

Drain first: no lane should run across a binary swap. Check
`provenant status`, collect `provenant output TARGET`, and only then `retry`
unfinished work.

If installation fails after stopping agents, the output names the stopped
agents and those that may still be running, prints the quoted state directory
and asks you to rerun your original install command with its original options
(`--source`, `--state-dir`, `--bin-dir`). Agents restart after a successful
install.

`provenant upgrade` preflights every configured host before installing any
host and never prompts while capturing installer output. Keep the fleet
quiescent for the whole operation; preflight cannot make a multi-host install
atomic.

### Test a candidate binary

Run an uninstalled candidate only with its own `--state-dir`, with
`PROVENANT_ROOT` set to its checkout. A candidate can change persisted state;
never point it at the state of an installed binary. `install --source DIR --upgrade-preflight`
is read-only and safe on real state.

## Build a staged release

Only for `--source` installs and candidate releases. Build outside a Fabric
lane, with a fresh absolute output directory outside every checkout (its parent
must exist). Never use a lane cache for a host build.

```sh
cd tools/front-door
export CARGO_TARGET_DIR="$(mktemp -d)/cargo-target"
export RUSTC_WRAPPER=.cargo/rustc-wrapper.sh    # reproducible path remapping
RELEASE="$(mktemp -d)/release"
cargo build --release --locked --bin provenant
cargo run --locked --bin package-release -- --out "$RELEASE"
unset CARGO_TARGET_DIR RUSTC_WRAPPER
```

The packager refuses an existing output directory, asset drift, or a binary
whose embedded source digest differs from the current source: rebuild after any
source change. `--binary PATH` packages another build.

## Install

Quit Claude Code and any other selected client or config editor before the
preview, and keep them closed through `check install`; Claude rewrites
`~/.claude.json` while running. Claude accounts live in exactly
`~/.claude-$LABEL`; sign in there before registering. `~/.claude` is a sign-in
source, never an account.

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

Repeat `--harness` per client (`claude`, `codex`, `cursor`, `agy`, `opencode`)
and `accounts register` per account (`--adapter codex` for Codex). Dispatch
needs registered accounts, and `quota tick` initialises quota state.

- Read the dry-run: a failed preview prevents writes.
- Hooks stay off on a first install. `--hooks on` registers them (Claude
  today; rerun after an upgrade to pick up new handlers, see
  [Claude hooks](claude-hooks.md)). Omitting `--hooks` keeps owned hooks and
  refreshes unchanged ones. `--hooks off` removes unchanged ones and releases
  edited ones. `--hooks on` refuses an edited Claude hook: run `--hooks off`
  first.
- Occupied or foreign entries are refused. If the preview shows an exact match
  you accept, rerun with `--adopt-existing`.
- A directory that is not a registered account needs `install --harness claude`
  with `CLAUDE_CONFIG_DIR` set (`CODEX_HOME` for Codex); each directory is a
  separate target in the one receipt.
- `check install` compares binary, assets, receipt and owned entries for every
  target without repairing. After a partial-write error, rerun `install`.

## Account sync

Every registered account directory holds the same managed skill links,
managed hooks and allow-listed shared Claude settings. `install`,
`accounts register` and `accounts sync` apply it. Credentials, identity fields
and user-owned entries are never synced.

An edited managed hook is left in place and reported as
`relinquished_modified_entry`; an account with no managed hooks has
relinquished them until `--hooks on`.

### Shared settings

Configure shared settings under `[domains.accounts.shared_settings]`; inspect
available keys with `provenant config schema`. Credentials and identity are not
synced. If `provenant check install` reports a settings conflict, use
`provenant accounts sync --adopt ACCOUNT` to make that account the source, or
`provenant accounts sync --force-settings` to replace its values from the source.
Preview with `provenant accounts sync --dry-run` first.

### Repair and drift

Run `provenant check install` and follow the reported repair. For account drift,
preview `provenant accounts sync --dry-run`, resolve any unmanaged conflicts,
then run `provenant accounts sync`. After a partial write, rerun the command.

## Background agents

On macOS, the default-state install manages peer and tick LaunchAgents. Run
`provenant hosts doctor` to check them. If a modified plist blocks install, move
it aside, boot out the affected agent with `launchctl`, and rerun install. Check
the tick output and result if it reports an error. See [Fabric hosts](fabric-hosts.md#macos-peers-and-the-login-keychain)
for peer setup and [GC](gc.md) for scheduled collection.

On Linux, the default-state install enables a user timer. If it must run while
logged out, enable lingering for the user with `loginctl enable-linger $USER`.

## Uninstall

```sh
provenant --state-dir "$STATE" uninstall --harness claude
```

This removes only entries that still match the receipt, never recursively, and
keeps state and release assets. With `CLAUDE_CONFIG_DIR` or `CODEX_HOME` set it
removes that directory's target only. Removing the last target also stops the
LaunchAgents or timer; a modified plist or unit is kept and reported, and a
failed bootout keeps the plist for a retry.

If the home or state root moved since install, uninstall reports
`relocated_stale_entry` (and `state_root_moved`) and never touches the old
paths. Finish by hand: `launchctl bootout gui/$UID/dev.provenant.peer`, move the
old peer plist aside, delete links under `~/.local/bin`, the harness skills
directories and instruction links that point into the old state root, then
rerun `install --adopt-existing`.

## Receipt recovery

A receipt that fails to decode reports a reinstall error. Move
`<state>/install/receipt.json` aside, preview, and reapply `install`; exact
existing files and links are no-ops. Links into an older release conflict:
remove those binary, skill and instruction links first. Use `--adopt-existing`
for a regular executable that must be replaced.
