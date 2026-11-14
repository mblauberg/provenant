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

Exit codes: 2 invalid arguments or interrupted prompt, 3 toolchain needs
consent, 4 download or checksum failure, 5 build or package failure;
installation errors keep the CLI's exit code.

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

### From 1.x to 2.0

2.0 keeps its state in a fresh root, `~/.local/share/provenant/v2`. It never
writes, moves or deletes the 1.x root, `~/.local/share/provenant/v1`, and
refuses any state root inside it. The first install at the default root, while
that root is absent or empty and a 1.x root exists, stops the 1.x launch agents
and then copies the owner configuration and nothing else:

- `config.toml`: accounts, routing, presets, Fabric peers, lanes and the shared
  settings source.
- `host-id`: this host's Fabric identity, which its peers name.
- `install/receipt.json`: the recorded targets, binary directory and hooks, so
  the install replaces the 1.x links instead of refusing them as foreign.

Run records, stores, ledgers, quota and usage history, worktree records and
caches stay in the 1.x root; credentials stay with each provider. The JSON
result names the copy under `state_copy` (`from`, `to`, `files`, `copied`) and
the human output says it in one line. A dry run or `--upgrade-preflight` reports
the copy it would make, checks the release and changes nothing. A failed agent
stop copies nothing. A non-empty 2.0 root is never seeded again, and an explicit
`--state-dir` or `PROVENANT_STATE_DIR` other than the default is never seeded.

The 1.x and 2.0 agents share the labels `dev.provenant.peer` and
`dev.provenant.tick` and their plist paths, so the install stops them by label
(with the retired `dev.provenant.disk-guard` and `dev.provenant.gc`) before the
copy and starts the 2.0 agents after it. `install.sh` maps a `--state-dir`
naming the 1.x root, which a 1.x `provenant upgrade` passes, to the 2.0
default.

On each host, peers included: land or park lane work and finish every lane
(1.x run records do not carry over), close other sessions, run `provenant
upgrade` (1.x) or `install.sh --upgrade` (2.0), then `provenant check install`.

Rollback: run `provenant uninstall` with the 2.0 binary, then the 1.x installer
(`install.sh --tag v1.11.2`, or the last 1.x tag), which finds its 1.x root
intact. Delete the 2.0 root by hand if you no longer want it.

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

```toml
[domains.accounts.shared_settings]
source = "/Users/me/.claude/settings.json"   # default: ~/.claude/settings.json when it exists
keys = ["permissions", "env", "statusLine", "enabledPlugins",
        "outputStyle", "includeCoAuthoredBy", "cleanupPeriodDays"]   # the default allow-list

[domains.accounts.entries.claude-work.settings]   # wins over the source for this account
outputStyle = "verbose"
```

Only listed keys sync, and with no source and no per-account settings nothing
does. The deny rule refuses identity and credential keys (`apiKeyHelper`,
`forceLoginMethod`, anything matching token, secret, password, oauth,
credential and similar words, in key or `env` names) and `hooks`, even when
listed; it errs towards false positives, names the key and rule, and stops
install, upgrade and sync but never uninstall.

Each synced key is recorded in the receipt. On the next sync an unedited
account follows the source. An account that edited a value is a
`settings_conflict` and is never overwritten. Resolve it with
`provenant accounts sync --adopt <account>` (copy its values into the source,
then sync all) or `--force-settings` (overwrite with the source's values). If
the source changed during `--adopt`, nothing is written: rerun.

### Repair and drift

`provenant accounts sync [--dry-run] [--prune-orphans]` reapplies the installed
release to every account and recorded target. A conflict in any target (an
unmanaged file where a link belongs) refuses the
whole command before any write: fix it or use `install --adopt-existing`, then
rerun. A mid-write failure lists completed and remaining actions; rerun.
`accounts register` reports `ACCOUNT_SYNC` with the same repair and keeps the
registration. `--prune-orphans` removes managed content of an unregistered
account (`orphaned_account`).

Drift is reported per account by `provenant check install`, `provenant accounts
list` and `provenant hosts doctor`: `unsynced_account`,
`missing_account_directory`, `unusable_account_directory`,
`account_missing_entry`, `settings_drift` and `settings_conflict`; each message
names the repair. A missing account directory is skipped, never recreated.

`~/.agents/AGENTS.md` is a link into the release's instructions. A plain file
in the way is user content: install refuses it. `check install` reports
`stale_shared_instructions` (run `provenant install`) or
`unmanaged_shared_instructions` (run `provenant install --adopt-existing`, or
move the file aside).

## Background agents

On macOS, an install into the default state root also manages two LaunchAgents
in `~/Library/LaunchAgents`: `dev.provenant.peer.plist` and
`dev.provenant.tick.plist`. The tick runs `provenant tick` every 60 seconds:
due graph deadlines, plus daily [GC](gc.md) of registered projects (result in
`<state>/tick/gc-result.json`; `[gc] schedule = false` stops scheduled
collection only). A modified plist is kept and blocks install: move it aside,
run `launchctl bootout gui/$UID/dev.provenant.peer` (or `.tick`), and rerun
install. After an upgrade, confirm the tick is loaded with
`launchctl print gui/$UID/dev.provenant.tick`; `launchctl kickstart -k` it if
not. `provenant hosts doctor` shows its last exit; an idle exit 0 is normal. Exit 3
means `provenant tick` reported an error in graph or scheduled work, or failed
before it could complete the tick (including a result-write failure). Inspect
that run's tick output and result to identify the underlying cause; upgrading
only repairs installation drift.

On Linux, a default-state install writes `provenant-tick.service` and
`provenant-tick.timer` under `$XDG_CONFIG_HOME/systemd/user` (else
`~/.config`) and enables the timer. It needs a login session; run
`loginctl enable-linger $USER` to keep it running logged out. There is no peer
agent on Linux.

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
Install, uninstall and `check install` ignore pre-1.0 entries and Git
launchers; remove those by hand.

## Releasing

A `v*` tag on a commit reachable from `origin/main` triggers
`.github/workflows/release.yml`, which builds, ad-hoc signs, packages and
checksums the arm64 release. Commit `docs/release-notes/<tag>.md` first; it
becomes the Release body.
