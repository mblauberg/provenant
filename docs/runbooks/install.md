# Build, package and install
Owner: provenant-config
Anchor: tools/front-door/crates/config/src

The `provenant-config` crate owns release stamping, packaging and managed-file
installation. See the [state design](../specs/state.md) for
the full contract. A staged release contains a verified manifest and immutable
assets. Installation publishes its receipt last, and exact desired bytes and
links are no-ops, including after a crash before receipt publication.

## Bootstrap and upgrade

`./install.sh` downloads the release matching an exact checkout tag, or the
latest release otherwise. `--tag vX.Y.Z` selects a release explicitly. Packages
are named `provenant-<target>.tar.gz` with a matching `.tar.gz.sha256` file;
the script verifies SHA-256 before unpacking and running the packaged binary.
The release workflow publishes only `aarch64-apple-darwin`; other hosts fail
before downloading. Build a checkout with `./install.sh --source --yes`
(`--from-source` is also accepted).

`--from-source [DIR]` uses that checkout's pinned Rust toolchain and asks before
installing a missing toolchain through rustup. `--yes` authorises this step
without a prompt. Cargo's target and build directories live in a temporary
directory that is removed on completion or failure. Manual builds are described
below.

The defaults are `~/.local/share/provenant/v1` for state and `~/.local/bin`
for the binary. `PROVENANT_STATE_DIR`, `--state-dir` and `--bin-dir` support
other paths. Existing install configuration still applies.

On a terminal, a first install without `--yes` offers provider selection,
pre-selecting detected CLIs and Claude account directories. It asks whether to
replace, append to or skip each instruction file, enable Claude hooks and
register the discovered Claude accounts. Append copies the release's instruction
text into the existing file; later upgrades preserve that user-owned text.
Skip preserves existing text or creates an empty user-owned file. Replace lets
the installer create its managed link. Prompts read the controlling terminal,
including when the script is piped into `sh`; `--yes` and receipt reruns skip
guided setup. A truncated script runs nothing, and child commands cannot consume
the shell's input. After the first successful install, a later account failure
preserves the published instruction links and receipt.

A bare `provenant install --source "$RELEASE"` detects executable `claude`,
`codex`, `cursor-agent`, `agy` and `opencode` commands on PATH and every
`~/.claude-*` directory. It reports each target, emits findings only when
present, and inspects the installed release at the end. `--dry-run` previews
without writing; `--json` keeps one result envelope. Explicit `--harness`
selects particular clients. Without it, an existing receipt determines the
exact targets and binary directory, ignoring newly detected clients and
environment overrides. Hook ownership is retained when `--hooks` is omitted.
Every registered account directory is also a target, whatever the selection
(see [account sync](#account-sync)).

```sh
provenant upgrade
provenant upgrade --from-source /absolute/path/to/provenant --hosts all
```

Drain writer lanes on each host before upgrading: let running providers exit
and settle before installing the release, then launch new writers afterwards.
The host does not force-cancel running lanes or retain a legacy object verifier.
An older interrupted settlement can resume through `provenant reconcile --id RUN`
when its private object directory remains. A missing private source is refused
with instructions to recover commits on the host and start a fresh run. See
[lane Git and import](../specs/run.md#lane-git-and-import).

Upgrade requires a receipt. It downloads one release tag for this host and all
registered Fabric peers, or builds the selected source checkout on every host.
It uses Fabric's SSH executable, destination and connection timeout, and each
host's own receipt settings. Release upgrades verify each host's package checksum
before running its bundled `scripts/install.sh`. Source transfers contain tracked
working files; peers build a temporary indexed checkout. Keep source changes stable during
the upgrade. Every host needs build tools for a source upgrade. Peer receipts
must be in their default state directory or selected by the peer's login
environment. Results identify each host; any failure makes the command fail.
Hosts are upgraded sequentially, so a failure can leave different versions.
Fix the reported failure and rerun; there is no distributed rollback.

Bootstrap exits 2 for invalid arguments or interrupted prompts, 3 when a
missing toolchain needs consent, 4 for a failed download or checksum, and 5
for a failed build or package. Installation errors retain the CLI's exit code.

Tags matching `v*` trigger `.github/workflows/release.yml`. The tag commit must
be reachable from `origin/main`. A job with read access builds and ad-hoc signs
the arm64 binary, runs `package-release`, and checksums the archive. A separate
publish job verifies the transferred bytes and publishes the archive and checksum;
only its publish step receives `GITHUB_TOKEN`. Before tagging, commit
`docs/release-notes/<tag>.md`; that file becomes the GitHub Release body.

## Build a staged release

On the host, outside a Fabric lane, build the binary and run the build-only Cargo example.
Choose a fresh absolute output directory outside every repository checkout. The
output's parent directory must already exist. The example copies the exact asset
inventory digested by `build.rs`, reads the binary's `--version --json` identity,
and runs the install path's bounded version and help smoke before publishing
`release.json` last. It refuses existing output directories and asset drift.
Cargo does not install this example.

```sh
cd tools/front-door
export PATH="$HOME/.cargo/bin:$PATH"
CARGO_PARENT="$(mktemp -d)"
export CARGO_TARGET_DIR="$CARGO_PARENT/cargo-target"
export RUSTC_WRAPPER=".cargo/rustc-wrapper.sh"
RELEASE_PARENT="$(mktemp -d)"
RELEASE="$RELEASE_PARENT/release"
cargo build --release --locked --bin provenant
cargo run --locked --example package-release -- --out "$RELEASE"
rm -rf -- "$CARGO_PARENT"
unset CARGO_TARGET_DIR CARGO_PARENT RUSTC_WRAPPER
```

Host builds use a fresh target directory. Confined writers use the separate
`$HOME/Library/Caches/provenant-lane-cargo/<sha256>` namespace, derived from
verified worktree and repository identities. It also holds the lane's private
Cargo home, npm and pnpm caches and Corepack home; the host's `~/.cargo/registry`,
`~/.npm`, `~/Library/pnpm` and similar stores are read-only to lanes. Lanes run the
installed pnpm whatever version a project pins (`manage-package-manager-versions`
and `package-manager-strict` are off), because a downloaded `@pnpm/exe` launcher
needs a hard link that lanes cannot create. Never use
a lane cache for a host build. `provenant worktree remove` deletes only the
removed worktree's own cache while holding its writer lock. Other orphaned
caches remain until `provenant gc [--dry-run | --apply] [--idle-days N]
[--free-floor SIZE]`, which also
removes caches idle for N days (default 7) for the selected project's
worktrees, sparing live or unobservable writers. Idle removal takes the existing
writer lock without waiting and releases it immediately after the tombstone
rename. A held lock keeps the cache as live; a missing or unsafe lock, a
reservation, or unverifiable registration keeps it as unobservable.
Bare `gc` deletes eligible caches and expired provider state; `--dry-run`
previews all cleanup. `--apply` also deletes eligible merged worktrees and
declared output. Scheduled GC also
removes clean merged worktrees using fetched upstream ancestry and prunes
explicitly configured regenerable output for known terminal lanes. The
[configuration runbook](configuration.md#lane-cache-collection) describes
per-project keep paths, the 24-hour write protection and low-space alerts.
If `--free-floor 20G` (or plain GiB) is set and space on the cache volume
remains below that floor after the usual pass, GC removes further eligible
caches oldest-first by their newest recursive write. It rechecks writer
exclusion and reservations before detachment, and stops at the floor or when
candidates run out. JSON adds `free_floor` removal reasons,
`free_bytes_before` and `free_bytes_after`. Dry-run predictions include
estimated reclaimable bytes and set `free_bytes_estimated` to true.
V1 owner markers support orphan cleanup and foreign-owner reporting, but never
idle cleanup because they lack device and inode identity. Existing owners
outside that project are reported as kept (foreign). Before deletion, GC
atomically renames each cache to a tombstone so a launching lane can create a
fresh cache. Later sweeps retry interrupted tombstones. Entries that fail
identity verification are quarantined under `.gc-refused-*` and kept for
inspection. The owner marker is deleted last, and a failed sweep reports
completed removals and bytes already freed alongside the error.

GC also collects terminal provider state, preserving transcripts and evidence.
See the [GC runbook](gc.md) for resume windows, credential expiry and dry runs.

The packager defaults to `$CARGO_TARGET_DIR/release/provenant`, or the
workspace's `target/release/provenant` without that variable. Pass
`--binary PATH` to package another release build, for example one built with an
explicit target triple. Run `cargo build --release --locked --bin provenant`
after source changes and before packaging. The packager refuses a binary whose
embedded source digest does not match the current build's source digest.

## Install

Bootstrap from the binary in the staged release. An explicit state root must
be absolute; the default is `~/.local/share/provenant/v1`. Quit
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
`quota tick` afterwards to initialise quota state. Registering an account also
installs the managed skills, MCP entry and hooks into its directory when an
install exists (see [account sync](#account-sync)).

Review every planned change before applying it; a failed preview prevents
writes. `--dry-run` creates no state root, lock or receipt. A first install
leaves hooks disabled unless requested. Re-install without `--hooks` keeps
ownership of previously installed hooks and refreshes unchanged entries for the
new installation. Edited hooks stay in place, leave the receipt and produce
findings. Missing hooks also leave the receipt. Hooks are one choice for every
target of a harness: a target that owns no hooks yet, such as a newly registered
account, takes the full set while another target of the harness owns them.
`--hooks off` removes unchanged owned hooks from every selected target and
relinquishes modified ones, reporting them. Explicit
`--hooks on` refuses an edited Claude hook; use `--hooks off` to relinquish its
ownership before reinstalling.
`--hooks on` registers hooks for each selected harness that has a
fixture-backed hook specification (Claude today). The Claude set includes the
session and worktree hooks described in the
[Claude hooks runbook](claude-hooks.md); rerun with `--hooks on` after an
upgrade to register new handlers. At least one selected
harness must have a specification. Unmanaged paths are preserved, and
conflicts are refused unless
`--adopt-existing` is given for files that match exactly.

To install into a Claude config directory that is not a registered account,
rerun `install --harness claude` with `CLAUDE_CONFIG_DIR` set; its MCP entry
then goes to that directory's `.claude.json`. Each directory is a separate
target in the one receipt, so runs accumulate. The same applies to Codex with
`CODEX_HOME`. Registered accounts need no such run.

`check install` compares the installed binary, assets, receipt and owned
selectors of every recorded target, without repair, and reports account drift
(see [account sync](#account-sync)). Reapply `install` after resolving a
partial-write error.

## Account sync

Every registered account directory holds the same managed content. Four things
are synced:

- the managed skill links;
- the Fabric MCP entry (in `.claude.json` for Claude, `config.toml` for Codex);
- the managed hooks, when any target of the harness owns them;
- the allow-listed shared Claude settings (below).

`provenant install` (including each upgrade), `provenant accounts register` and
`provenant accounts sync` all apply it, so a new or upgraded install leaves the
accounts identical. An account directory is `~/.claude-<label>` for Claude and
the `--directory` given at registration for Codex, the adapters whose accounts
are separate configuration directories. Registered directories are resolved to
their canonical path (symlinks, relative and `..` spellings) and two
registrations of one directory count once. Cursor, Agy and OpenCode accounts
are local logins of one client directory, so their registrations add no target.
`~/.claude` is a target when installed, but never a registered account.

Never synced: credentials and sign-in state (`.credentials*`, `auth.json`),
identity fields in `.claude.json` such as `oauthAccount`, any `settings.json`
key outside the allow-list, user-owned hooks and MCP entries, and the key
names refused by the deny rule in
[configuration](configuration.md#chair-account-switching) (`apiKeyHelper`, any
name containing `token`, `secret`, `session` and the like), which are refused
outright. Other
bytes in `settings.json` and `.claude.json` are preserved.

### Hooks

An edited managed hook is left in place and reported as
`relinquished_modified_entry`. An account that removes every managed hook has
relinquished them: the removal persists across syncs and is not drift, and
only `--hooks on` brings them back. A target that never had hooks, such as a
new account, takes the hooks its siblings own.

### Shared settings

`[domains.accounts.shared_settings]` names a `source` settings file (default
`~/.claude/settings.json` when it exists) and the allow-list `keys`
(default `permissions`, `env`, `statusLine`, `enabledPlugins`, `outputStyle`,
`includeCoAuthoredBy`, `cleanupPeriodDays`). `[domains.accounts.entries.<id>.settings]`
holds per-account values that win over the source. See
[configuration](configuration.md#chair-account-switching).

Each synced key is recorded in the receipt (`setting:<key>`) with the value
last written. On the next sync:

- an unedited account follows the source, including a key the source dropped;
- an account that edited the value, or held a different one before the first
  sync, is a `settings_conflict` naming the account and key, and is not
  overwritten;
- `provenant accounts sync --adopt <account>` copies that account's edited
  allow-listed values into the source file, then syncs every account;
- `provenant accounts sync --force-settings` overwrites the edits with the
  source's values;
- a key the account deleted stays deleted (`--force-settings` restores it).

Sync never rewrites the source file or the account that holds it; `--adopt`
is the one explicit exception. It resolves a symlinked source and writes the
target, so the link survives, stages the new file exclusively beside it, and
re-reads the source just before the swap: if the bytes changed since the plan
read them, nothing is written and you rerun the command. Applying settings to
an account likewise compares the planned snapshot before its atomic rename, so
an edit saved by a running Claude session in between is reported as drift
rather than lost. Settings drift is
reported as `settings_drift` or `settings_conflict` by `check install`,
`accounts list` and `hosts doctor`.

### Repair and drift

`provenant accounts sync [--dry-run] [--adopt <account>] [--force-settings]
[--prune-orphans]` is the repair command. It reapplies the installed release to
every registered account directory and recorded target, and needs an install
receipt. A conflict in any target (an unmanaged file where a managed link
belongs, or a foreign `fabric` MCP entry) refuses the whole command before any
write; fix or adopt it (`install --adopt-existing`), then rerun. A failed
write part way through is reported as a partial install listing exactly the
actions completed and remaining; rerun the same command. Registration is never
undone by a failed sync: `accounts register` reports `ACCOUNT_SYNC` and the
same repair. A configuration defect stops install, upgrade and sync but never
`uninstall`, which ignores the configuration and says so.

When accounts of a harness are registered, a recorded target whose account was
unregistered is reported as `orphaned_account` and not synced.
`--prune-orphans` removes its managed content.

Drift appears in three places, each per account:

- `provenant check install` adds `unsynced_account` (registered, never
  installed), `missing_account_directory`, `unusable_account_directory`,
  `account_missing_entry` (the account lacks a skill, MCP entry or hook that a
  sibling account of the harness owns), `settings_drift` and `settings_conflict`
  to its `owned_entry_drift` findings. Each message names the account and the
  repair.
- `provenant accounts list` adds a `state` per account: `synced`, `drift`,
  `unsynced`, `missing_directory` or `not_installed`, with the findings in
  `data.sync`.
- `provenant hosts doctor` reports the same `managed` rows beside the sign-in
  rows, locally and for a peer; with no install receipt it lists none.

An account whose directory is gone is skipped and reported (also by
`accounts register`), never recreated.

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

The same macOS default-state install also manages
`~/Library/LaunchAgents/dev.provenant.gc.plist`, with `RunAtLoad = true` and
`StartInterval = 86400` (daily). It runs `provenant gc` for registered projects,
protecting live writers and reservations. Its stdout and stderr go to `<state>/gc/`.
`[gc] schedule = false` disables scheduled deletion; rerun install to remove
the owned agent. `idle_days` becomes an agent argument on install. The collector reads
`gc.free_floor` from configuration each run. When `free_floor` is unset, the floor is 10% of the state volume,
at least 50 GiB. See [configuration](configuration.md#lane-cache-collection).

Install quietly boots out and deletes an existing
`~/Library/LaunchAgents/dev.provenant.disk-guard.plist` on upgrade. Disk checks
now run inside active lane supervisors. There is no periodic disk-guard agent.
After bootout, install waits up to three seconds for the checker's recovery
lock, resumes saved builds after checking their process identities, then removes
the recovery state and plist. Stopping the checker first prevents another pause
during recovery. Uninstall also performs this one-time retirement when an
installation was last managed by an older release.
See [configuration](configuration.md#disk-pressure) for the shared floor.

To remove an installation, run
`provenant --state-dir "$STATE" uninstall --harness claude`. It removes only
unchanged owned entries and keeps the state and release assets. It never
recursively deletes a checkout, state root or release. With `CLAUDE_CONFIG_DIR`
or `CODEX_HOME` set, uninstall removes only that directory's target; without
it, every recorded target of the harness.
Removing the last recorded target also boots out the peer and GC
LaunchAgents and removes their unchanged plists.
A modified plist is kept and reported; its
service is left running. A failed bootout keeps the plist for a retry.
A missing recorded plist is stopped by its service label.

If your home directory or state root has moved since install, uninstall reports
one `relocated_stale_entry` per entry it will not touch, plus `state_root_moved`
if the state root moved, then forgets the removed targets. This also applies
with a custom `PROVENANT_STATE_DIR` or state outside the home. It never acts on
the old paths. Human output also says `Home moved; old paths left untouched`
when the state root stayed in place. To finish,
run `launchctl bootout gui/$UID/dev.provenant.peer`, move
`~/Library/LaunchAgents/dev.provenant.peer.plist` aside, and delete links under
`~/.local/bin` and the harness skills directories, plus harness instruction
links such as `AGENTS.md` and `HARNESS.md`, that point into the old state root.
Then rerun install with `--adopt-existing` to replace managed MCP entries that
still name the old binary and state. Stopping the agent and removing these
links remain manual steps.

`build.rs` stamps the source and asset digests, and the packaging example
produces the typed `release.json`. The CLI grammar is described in
[cli.md](../specs/cli.md); `help --json` describes the command
surface without resolving roots or creating state.

## Receipt recovery

A receipt that fails to decode reports a reinstall error. A `legacy` member of
any value is ignored and dropped on the next install write, so it needs no
recovery. For any other invalid receipt, move `<state>/install/receipt.json`
aside, then preview and reapply `install` with the desired release and
harnesses. Existing exact desired files and links
remain no-ops. Links into an older release conflict; remove those binary, skill
and instruction links manually before reapplying install. Use `--adopt-existing`
for occupied managed MCP selectors or a stable regular executable that must be
replaced, and inspect the preview before applying it. Remove obsolete entries
manually; install, uninstall and `check install` no longer clean up or audit
pre-1.0 entries or Git launchers.

## Fabric MCP registration

`provenant fabric mcp [--client NAME]` is the explicit stdio transport. The
`--client` flag is optional; supported values are `claude`, `codex`, `cursor`,
`agy` and `opencode`. `--json`
is refused before stdin. A bare `provenant` prints usage and exits 2, so a
client registration must name `fabric mcp`.

Installation registers the transport for each selected harness and preserves
unrelated client configuration; there is no separate registry editor. See the
[Fabric design](../specs/fabric.md).
