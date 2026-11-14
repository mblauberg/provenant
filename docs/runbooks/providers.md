# Providers
Owner: provenant-providers
Anchor: [`providers tests`](../../tools/front-door/crates/providers/src/tests.rs)

Owner-side setup that a provider's launch refusal points here for. Adapters,
capabilities and routes are listed by `provenant route list`; confinement and
launch behaviour are in the [supervisor spec](../specs/supervisor.md).
`provenant quota` names the recovery command for an account that needs a login.

## Claude lane tokens

A renewed Claude login can revoke a token copied into a running lane. To avoid
interrupting work, register a separate lane token for each account.

To remove the cause, register a long-lived lane token per account. It comes
from its own grant, so renewing the login never revokes it:

1. Sign in to the account the directory is registered for and create the token:
   `CLAUDE_CONFIG_DIR=<account config dir> claude setup-token` (directory from
   `provenant accounts list`; omit the variable for `~/.claude`).
2. Store it in that directory as `provenant-lane-token`, readable only by you,
   without putting it on argv or in shell history:

   ```sh
   cd <account config dir>
   ( umask 077; read -rs token; printf '%s\n' "$token" > provenant-lane-token )
   ```

3. Launch a Claude lane on the account. A file that is a link, has more than
   one hard link or is group- or world-readable refuses the launch.

Provenant cannot check that the token belongs to the registered account. It
lasts about a year; revoke it in the account settings on claude.ai, or revert by
deleting the file. Never copy it between account directories.

Register each account with its own CLI login; do not copy refresh tokens.
Use `provenant quota` to inspect account health and follow its recovery command.

## Check provider routes

Before and after a release, run `provenant check providers --json` against the
candidate's isolated state and then the installed state. Inspect the readiness
report. With authority to spend quota, run `provenant check providers --live`
to exercise routes; use `provenant check providers --help` to select routes.
Retain the JSON and attempt IDs as release evidence. Investigate failed routes
and rerun the check after repair.

## Agy accounts

Agy has no account switch, so a second Google account gets its own `HOME`
holding its own login keychain, and the registered `directory` is that home.
Register the account signed in on your own login keychain with the home
directory itself:

```sh
provenant accounts register --adapter agy --directory ~ LABEL
```

If the isolated keychain or identity is unavailable, repair the account and
re-register it, then check `provenant quota` and `provenant hosts doctor`.

Add a second account on macOS (the empty keychain password and no lock timeout
are deliberate, so a headless lane never prompts):

1. `D=~/.agy-second; mkdir -p $D/Library/Keychains; chmod 700 $D`
2. `HOME=$D security create-keychain -p '' $D/Library/Keychains/login.keychain-db`
3. `HOME=$D security set-keychain-settings $D/Library/Keychains/login.keychain-db`
   then `HOME=$D security unlock-keychain -p '' $D/Library/Keychains/login.keychain-db`
4. `HOME=$D agy`, run `/login`, finish the browser sign-in with the second
   Google account, exit.
5. Check `HOME=$D agy -p /usage --output-format json` returns `SUCCESS`.
6. `provenant accounts register --adapter agy --directory $D LABEL`, then
   `provenant quota`.

The keychain file is plaintext-equivalent credential material: keep the
directory at mode 700 and out of backups and synced folders. It locks again
after a reboot: repeat the unlock in step 3. If sign-in is lost, repeat step 4.
A lane on the shared login account can read the host `login.keychain-db`
(encrypted) so Agy works.

## OpenCode

OpenCode uses the real HOME and XDG config. The Console's free models use the
derived `zen` account, separate from the `go` subscription account and paced
from their own quota observations; a `[domains.routing.pacing.accounts.go]`
profile applies to Go lanes only.

## Command and host-native routes

See [routing and templates](routing-and-templates.md#templates) for route setup.

## Cloud sessions

See [Cloud launch](cloud-launch.md) to launch and collect a provider session.
