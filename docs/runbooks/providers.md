# Providers
Owner: provenant-providers
Anchor: [`providers tests`](../../tools/front-door/crates/providers/src/tests.rs); [`cloud.rs`](../../tools/front-door/crates/providers/src/cloud.rs)

Owner-side setup that a provider's launch refusal points here for. Adapters,
capabilities and routes are listed by `provenant route list`; confinement and
launch behaviour are in the [supervisor spec](../specs/supervisor.md).
`provenant quota` names the recovery command for an account that needs a login.

## Claude lane tokens

By default a Claude lane gets a copy of its account's login access token. When
the host renews that login (a launch within 15 minutes of expiry, a quota probe,
or any `claude` run on the account's config directory), the server can revoke
every copy and live lanes fail with `authentication_failed`. The lane settles
with cause `lane_token_lost`; a writer continues from its handoff on a fresh
token, bounded by `lane.max_continuations`. If the registered lane token itself
is rejected, the lane settles without continuation and names the account.

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

A separately registered directory can hold a copied single-use refresh token;
quota never rotates it, so sign in interactively there when `provenant quota`
reports `login_expired`.

## Agy accounts

Agy has no account switch, so a second Google account gets its own `HOME`
holding its own login keychain, and the registered `directory` is that home.
Register the account signed in on your own login keychain with the home
directory itself:

```sh
provenant accounts register --adapter agy --directory ~ LABEL
```

An isolated account fails closed: if its `Library/Keychains/login.keychain-db`
is missing, a symlink, hard-linked or the same inode as your own, registration,
launch and quota fail naming the path (`KEYCHAIN_UNAVAILABLE` in `provenant quota`
and `provenant hosts doctor`). A launch also skips the route on a changed Google
identity (`re-register it`). Re-registering or `provenant quota tick` re-probes
a repaired account at once.

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

Name argv for the model-less `command` adapter in host configuration, then
select it by name from a graph task:

```toml
[domains.fabric.commands.fmt-check]
argv = ["cargo", "fmt", "--check"]
runtimes = ["/opt/check/bin/python3"]   # optional: interpreters to allow executing
```

```json
{"id":"format","kind":"task","access":"read_only","prompt":"Run the configured format check.","route":{"adapter":"command","name":"fmt-check"}}
```

Prompt text never supplies argv. The result ends `VERDICT: PASS` for exit zero
and `VERDICT: FAIL` otherwise.

`host-native` selects a named host feature; the one kind, `claude-agent`, runs a
Claude Code agent with a definition from
`[domains.fabric.host_native.NAME]` (`kind`, `model`, `effort`, `description`,
`prompt`). A graph must list a later non-native route as fallback, for
example `"routes":[{"adapter":"host-native","name":"ci-fixer"},{"alias":"senior"}]`.
A launched writer that fails stops for recovery rather than starting another.

## Lane user skills and project paths

`domains.lanes.user_skills` is `"discover"` (default) or `"off"`; override per
project under `[domains.lanes.projects."PROJECT-ID"]` (ID from
`provenant fabric identity`) or per route with
`domains.routing.model_policy.rules`. A route policy beats the project, which
beats the host default. Host skills are copied read-only to an attempt-owned
tree, limited by `domains.lanes.skill_stage_max_bytes`.

Writers get extra read-write paths from `shared_paths` and execute-only
directories from `exec_roots` in the same project table (ignored entries warn);
neither changes PATH. For host-wide write grants use `[domains.run]
writer_writes` with real paths (`/private/tmp`, not `/tmp`).

## Cloud placement

Cloud jobs run verification or a remote writer. Install and log in to the
provider CLI and make sure the local `origin` is the intended repository; never
put credentials in prompts or placement settings. Pass the placement as a `--set`
on the default task template, with `--explain` first:

```sh
provenant run --file brief.md --mode read-only --explain \
  --set "attempt.cloud={target='codex-cloud',provider='codex',environment='ENV',repository='OWNER/REPO',branch='main',model='MODEL',deadline_seconds=1800}"
```

For hosted Claude use `target='claude-hosted',provider='claude'` and omit
`environment` (a `ccpool_...` ID selects a self-hosted pool). A writer takes
`--mode worktree-write`, a named `work_branch` that is not the base branch
(dispatch refuses otherwise), and a longer deadline. The job runs at the current
checkout's commit, so push that commit to `origin` first. Deliver a writer with
`provenant lane deliver ATTEMPT --deliver push`.

A writer job pushes to `work_branch`, never with force and never to the default
branch, and settles only after the reported head is verified on the remote and
descends from the starting commit (else `cloud writer push not verified`).
Required CI on that head still decides; a cloud result never replaces it. After
a restart `provenant reconcile --id ATTEMPT` observes saved jobs.

- Claude has no status CLI. The job's result commit message ends with
  `Provenant-Cloud-Result: <one-line JSON>` and a finished job settles at the
  next `reconcile`. Hosted Claude may allow only `claude/*` branch names. An
  unfinished job past its deadline becomes `cloud_unknown`; open the session URL
  it prints. A setup script tied to another repository stops the session before
  Claude Code starts and shows only as `cloud_unknown`.
- Codex returns its result in `.provenant-cloud-result.json`. If `codex cloud`
  lists no environments, set `[providers.codex.cloud] transport = "thread"` (and
  optional `command`, default `codex-cloud-task`) in the state-root `config.toml`.
- Neither provider confirms cancellation. Compare `observed_model_status` before
  relying on a requested model.

The job runs `scripts/cloud-verify` to record its result;
`scripts/cloud-environment [REPOSITORY]` prints a checklist for creating the
provider environment in the provider's interface.
