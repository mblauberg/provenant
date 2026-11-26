# Providers
Owner: provenant-providers
Anchor: [`providers tests`](../../tools/front-door/crates/providers/src/tests.rs)

Owner-side setup that a provider's launch refusal points here for. `provenant route
list` shows adapters, capabilities and routes, and `provenant quota` names the
recovery command for an account that needs a login. Confinement and launch
behaviour are in the [supervisor spec](../specs/supervisor.md). Route setup is in
[routing and templates](routing-and-templates.md#templates); cloud sessions are in
[cloud launch](cloud-launch.md).

## Claude lane tokens

A renewed Claude login can revoke a token copied into a running lane. A
long-lived lane token per account comes from its own grant, so renewing the login
never revokes it:

1. Sign in to the account the directory is registered for and create the token:
   `CLAUDE_CONFIG_DIR=<account config dir> claude setup-token` (directory from
   `provenant accounts list`; omit the variable for `~/.claude`).
2. Store it in that directory as `provenant-lane-token`, readable only by you,
   without putting it on argv or in shell history:

   ```sh
   cd <account config dir>
   ( umask 077; read -rs token; printf '%s\n' "$token" > provenant-lane-token )
   ```

3. Launch a Claude lane on the account. A file that is a link, has more than one
   hard link or is group- or world-readable refuses the launch.

Provenant cannot check that the token belongs to the registered account. It lasts
about a year; revoke it in the account settings on claude.ai, or delete the file
to revert. Never copy it between account directories. Register each account with
its own CLI login; do not copy refresh tokens.

## Codex configuration

`providers.codex.clean_config` (default false) set to true passes
`--ignore-user-config` for writer lanes. Read-only execution defaults to clean
configuration whatever the writer preference; an override warns with its source.
Private state and login isolation still apply; the setting grants no file access.

## Check provider routes

Before and after a release, run `provenant check providers --json` against the
candidate's isolated state and then the installed state, and read the readiness
report. With authority to spend quota, add `--live` to launch one tiny read-only
lane per route; `--route ADAPTER/ACCOUNT/MODEL` selects routes. Keep the JSON and
attempt IDs as release evidence, and rerun after repairing a failed route.
The readiness report includes sourced adapter overlays and declared caveats
even when the optional version-probe executor cannot start.

## Agy accounts

Agy has no account switch, so a second Google account gets its own `HOME` holding
its own login keychain, and the registered `directory` is that home. The login at
your own HOME is the adapter's local account and needs no registration. A
registered directory must not equal or contain HOME or overlap another registered
account directory.

Add a second account on macOS (the empty keychain password and no lock timeout are
deliberate, so a headless lane never prompts):

1. `D=~/.agy-second; mkdir -p $D/Library/Keychains; chmod 700 $D`
2. `HOME=$D security create-keychain -p '' $D/Library/Keychains/login.keychain-db`
3. `HOME=$D security set-keychain-settings $D/Library/Keychains/login.keychain-db`
   then `HOME=$D security unlock-keychain -p '' $D/Library/Keychains/login.keychain-db`
4. `HOME=$D agy`, run `/login`, finish the browser sign-in with the second Google
   account, exit.
5. Check `HOME=$D agy -p /usage --output-format json` returns `SUCCESS`.
6. `provenant accounts register --adapter agy --directory $D LABEL`, then
   `provenant quota`.

The keychain file is plaintext-equivalent credential material: keep the directory
at mode 700 and out of backups and synced folders. It locks again after a reboot
(repeat the unlock in step 3). If sign-in is lost, repeat step 4. If the isolated
keychain or identity is unavailable, repair the account, re-register it and check
`provenant quota` and `provenant hosts doctor`. A lane on the shared login account
can read the host `login.keychain-db` (encrypted) so Agy works.

## OpenCode

`lanes.access` selects the profile for OpenCode lanes. The default `host` points
OpenCode at the host's real XDG data, config, state and cache directories, which
include its credential stores, and attaches to the host service when one is
running. The service stores sessions, resumed by session ID; if the prior session
is missing the lane warns and starts a fresh one, and with no service it warns and
runs standalone on the host profile. Launch output records the effective profile
and its source.

When attached, OpenCode's tools run in the host service process with the attempt
worktree as their directory, so the lane's sandbox confines the client process, not
those tools. Standalone, the sandbox applies to the tools. `lanes.access =
"isolated"` uses a private per-attempt profile instead.

Lane launches pass `--archived`, so a lane session does not appear in the owner's
OpenCode session lists; the session ID stays addressable for a resumed lane.

The Console's free models use the derived `zen` account, separate from the `go`
subscription account and paced from their own quota observations; a
`[domains.routing.pacing.accounts.opencode-go]` profile applies to Go lanes only.
