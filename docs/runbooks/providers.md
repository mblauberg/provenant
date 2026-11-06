# Providers
Owner: provenant-providers
Anchor: tools/front-door/crates/providers/src

Per-adapter launch notes. The model adapters (Claude, Codex, Cursor, Agy and
OpenCode) are enabled by default in every access and session mode with a launch
form; recordings serve replay tests only. Use `provenant route list`
to inspect the catalogue; readiness is decided at dispatch.

`AdapterSpec` owns credential metadata watch files, authentication markers, login
hints, compaction environment controls and allowance prefixes. Credential watches
inspect metadata only. Recognised missing or expired provider logins settle as
`auth_required`, including when every candidate is skipped before launch.
Read-only graph nodes can then try their next declared route.

Quota probes use the selected Claude account's directory only and check its
registered fingerprint before and after reading the credential. A separately
registered directory can contain a copied single-use refresh token. Quota does
not automatically rotate it because it cannot establish independence without
reading another account's secrets. Sign in interactively in that directory
when it needs attention. Where the
registration is known, `provenant quota` gives the account-specific command;
otherwise it says the remedy is unavailable. A recorded expired credential is
reported as `login_expired`; a failed probe alone does not establish expiry.
Quota shows expiry as unknown when no fresh selected-account metadata is
available. `domains.quota.login_expiry_warning_seconds` sets the advance
warning window (default 3600 seconds; zero disables advance warnings). A quota
tick sends a deduplicated notice for a known expiring or expired login to each
registered local chair session. The notice contains the account label and expiry,
not credential contents or provider output; `provenant quota` supplies recovery.

## Launch capabilities

One declared table per adapter (`AdapterSpec::launch` in
`tools/front-door/crates/records/src/adapters.rs`) is the source for dispatch,
`run`, resume and graph validation. Unsupported enforced controls are refused with the
adapter and field named (`cursor does not support effort`) before admission,
worktree preparation, reservation or receipt creation. A named route is checked
once routing resolves its adapter; a model's recorded effort controls in the
catalogue still apply on top.

| Adapter  | Effort | Resume | Steering | Network modes          | Image input | Write |
| -------- | ------ | ------ | -------- | ---------------------- | ----------- | ----- |
| claude   | yes    | yes    | no       | provider, local, open  | yes         | yes   |
| codex    | yes    | yes    | no       | provider, local, open  | yes         | yes   |
| cursor   | no     | yes    | no       | provider, local, open  | no          | yes   |
| agy      | yes    | yes    | no       | provider, local, open  | no          | yes   |
| opencode | yes    | yes    | no       | provider, local, open  | no          | yes   |
| command  | no     | no     | no       | off, local, open       | no          | no    |
| host-native | configured | no | no | provider, local, open | no | yes |

Command routes have no model and refuse model, effort, resume and write
requests. Steering (input into a live session) has no request surface yet.
Repeat `run --attach PATH` to grant a lane read access to image files. Claude
receives their paths in the prompt; Codex receives image arguments. Other
adapters warn that `image_input` is unavailable while retaining the read grant.
Remote dispatch cannot transfer these files and warns when attachments are omitted.
Cursor has no effort control on its command line, so its
catalogue models record none.

## Host-native

`host-native` selects an optional named host feature. The first supported kind,
`claude-agent`, runs a Claude Code agent in print mode using the ordinary Claude
account, prompt, confinement, output decoder and writer delivery path. Configure
its model, effort and agent definition in the effective host configuration:

```toml
[domains.fabric.host_native.ci-fixer]
kind = "claude-agent"
model = "claude/claude-sonnet-5-5"
effort = "medium"
description = "Repair failing CI checks"
prompt = "Fix the checks named in the task, stay within its write scope and commit on its branch."
```

The launcher supplies this definition through `--agents` and selects it with
`--agent ci-fixer`. It does not depend on an agent file in the user's home.
Model and effort belong to this configured row; the task cannot override them.
Network defaults to open, as on ordinary model routes; explicit provider-only
or local reach and access remain the lane's declared permissions. Every launch
starts fresh: explicit `resume --no-fallback` refuses it, automatic policy retries are
disabled, and ordinary `retry` can dispatch a fresh lane with the prior result.
Receipts retain the actual provider/model/effort plus `host_native` and the
configuration `source` in `applied_route`.

A graph must list a later non-native fallback for every host-native route:

```json
{"id":"repair","access":"worktree_write","worktree":"/repo/.worktrees/fix","prompt":"Repair the failing checks and commit.","routes":[{"adapter":"host-native","name":"ci-fixer"},{"alias":"senior"}]}
```

An absent entry, unavailable provider or pre-launch refusal allows the next
graph route. An internal provider fallback to another model is refused, so it
cannot silently drop the selected agent or its contract. Once a writer has
launched, a failure stops it for recovery rather than
starting another writer. The bundled core path always uses ordinary routes;
see [the deliver repair example](presets.md#host-native-repairs) to opt in.

Vendor interfaces verified on 29 October 2026:

| Interface | Verified contract | Row support |
| --- | --- | --- |
| Claude Code named agent | Print mode, `--agent` and inline `--agents` definitions ([CLI reference](https://code.claude.com/docs/en/cli-reference), [subagents](https://code.claude.com/docs/en/sub-agents#invoke-subagents-explicitly)) | `claude-agent` |
| Claude Code `/autofix-pr` | Terminal command creates a cloud session and enables PR monitoring ([vendor docs](https://code.claude.com/docs/en/claude-code-on-the-web#auto-fix-pull-requests)) | No documented bounded headless invocation; no executable row shipped |
| Cursor Bugbot Autofix | Cloud agents can push to the PR branch or a separate branch; its own check reports success or neutral ([vendor docs](https://cursor.com/docs/bugbot#autofix)) | No documented bounded local fixer invocation; no executable row shipped |

Cloud fixer pushes must enter the normal CI gate on the observed PR head. A
fixer's completion or its own status check does not establish project CI. A
future asynchronous kind must reconcile the branch and resulting commit before
it can satisfy the writer delivery contract; the CI gate still checks that head.

## Command

Configure named argv in the host's effective configuration:

```toml
[domains.fabric.commands.fmt-check]
argv = ["cargo", "fmt", "--check"]

[domains.fabric.commands.python-check]
argv = ["./scripts/check.py"]
runtimes = ["/opt/check/bin/python3"]
```

A graph task selects the entry by name:

```json
{"id":"format","kind":"task","access":"read_only","prompt":"Run the configured format check.","route":{"adapter":"command","name":"fmt-check"}}
```

The dispatch manifest and Fabric run wire route is
`{"kind":"adapter","provider":"command","name":"fmt-check"}`. This generic
adapter route leaves room for other named, model-less adapters. It carries no
model, effort or account. Prompt text never supplies or changes argv. Bare
executables resolve on the host's captured PATH; paths resolve from the lane's
working directory. A single task needs no manifest:
a deterministic action template bound to `command/fmt-check`. The applied
route records `domains.fabric.commands.NAME` as its configuration source, with no model in the receipt.

Commands use the ordinary supervisor, guardian, deadlines, descendant cleanup
and operation replay. They run read-only with the shared Rust and Node toolchain
grants and private build output. Network is on by default, as for every
lane. `--network off`, or `"network":"off"` on a manifest task (which overrides
the flag for that task), runs the command with the recorded `provider` mode,
which has no provider API endpoints for this adapter. All known credential stores,
including both GitHub CLI config directories and keychains, stay denied in
every network mode. Commands have no session, hooks, MCP configuration or
quota probe.

`runtimes` is optional and takes absolute interpreter paths. Each path is
resolved on the host and granted execution only, so a script can use a custom
shebang interpreter outside the system, Rust and Node toolchains. The grant
does not allow reading neighbouring files or writing the interpreter directory.
Exact non-system Mach-O dependencies receive the existing library read grants;
credential and project write denials still apply. List each interpreter needed
by the configured command or its child scripts.

Stdout and stderr share a bounded 64 KiB output tail. Captured lines are quoted
and log `verdict:` markers are escaped, so only the final exit-derived verdict
feeds a gate. The result ends with
`VERDICT: PASS` for exit zero and `VERDICT: FAIL` otherwise, including timeouts.
An ordinary verdict gate can consume a successful task's result; the gate
still has no attempt. A failed command fails its task. A missing executable
settles as adapter unavailable through the existing launch failure path.

## Provider credentials in lane shells

On macOS a Seatbelt profile applies to the whole process tree, so a shell
command a provider runs gets the same file grants as the provider itself.
OpenCode and Codex lanes can therefore read their own provider's credential:
OpenCode reads `~/.local/share/opencode/auth.json` for its sign-in and
connected API keys, and Codex reads and rewrites `~/.codex/auth.json` when it
refreshes tokens. This is an accepted limit. A lane is not granted other
providers' credential stores, and GitHub CLI configuration stays denied
([#2432](https://github.com/mblauberg/provenant/issues/2432)).

## Project paths and host checkout identity

Lanes receive `PROVENANT_PRIMARY_CHECKOUT`, the canonical primary checkout.
Git-backed lanes also receive `PROVENANT_WORKTREE_GITDIR`, their real,
read-only admin directory. A writer's `git rev-parse --git-common-dir` still
names its private Git view; use the environment variable to locate host-owned
project state instead of querying Git against the primary checkout.

Configure existing shared paths and repo-managed tools per registered project:

```toml
[domains.lanes.projects."PROJECT-ID"]
shared_paths = [".agent-run/locks", "~/shared-test-state"]
exec_roots = [".local-tools"]
```

`shared_paths` gives this project's writers read-write access. Paths may be
absolute, home-relative or relative to the primary checkout; missing paths are
never created. Read-only lanes get no additional writes. Git directories,
credential stores, protected host state and executable roots are omitted with a
`shared_path_ignored` warning.

`exec_roots` gives lanes read and execution access to directory subtrees under
the primary checkout. Entries that escape it, are missing, overlap writes or
name protected or credential paths are ignored with `exec_root_ignored`.
Neither setting changes the lane's PATH; call tools by a project-resolved path.
The receipt's `project_paths` records the applied paths and warnings with their
configuration keys. Paths use checkout- or home-relative forms; external paths
require inspection of the host configuration.

## User skills in lanes

Lanes discover host user skills read-only by default. The
`domains.lanes.user_skills` setting accepts `"discover"` (default) or `"off"`.
`provenant config show` and `provenant config schema` show its value, default
and origin. Set it in the selected state root's `config.toml`:

```toml
[domains.lanes]
user_skills = "off"

# Override the host default for a registered Fabric project ID.
# Find the ID with `provenant fabric identity`.
[domains.lanes.projects."PROJECT-ID"]
user_skills = "discover"

# Narrow discovery to particular routes with the existing model policy.
[[domains.routing.model_policy.rules]]
adapter = "claude"
[domains.routing.model_policy.rules.policy.lane]
user_skills = "off"
```

A supplied route policy wins over the project override, which wins over the
host default. Omit an override to inherit. A brief can still name or quote a
skill explicitly when discovery is off. Project skills remain in the worktree;
this setting controls host user skills only.

| Provider | Host user skill directory |
|---|---|
| Claude | `~/.claude/skills` |
| Codex | `~/.agents/skills` |
| agy | `~/.gemini/skills` |
| OpenCode | `$XDG_CONFIG_HOME/opencode/skills`, falling back to `~/.config/opencode/skills`; relative XDG overrides are ignored |
| Cursor | `~/.cursor/skills` |

The host copies selected skills to an attempt-owned tree before launch. The
lane receives read-only access to that tree, and the provider discovers the
staged copy. Links are followed into the copy, but targets in credential roots
are skipped with a warning. Missing directories are skipped. Files exceeding
the staging byte limit and entries that change during capture are skipped with
a warning. If the staged tree cannot be written, the lane continues without
user skills and records a warning. The staged tree contains no links and
receives no write grant.
`domains.lanes.skill_stage_max_bytes` sets the total capture limit (16 MiB by
default); the receipt records its effective value and configuration origin.
Project skills and selected-provider credential access keep their existing
rules. Reading a skill does not change nested dispatch policy.

Each attempt's applied route records `user_skills` with its requested mode,
whether skills were exposed, the home-relative source when available, staged
file count, warnings and setting source. `provenant output ATTEMPT --json`
includes that receipt; older attempts have no receipt.

## Claude network access

With open network (the default, or `--network on`), Claude enables `WebFetch`
and `WebSearch` in both its tool selection and permission allow-list. Read-only
lanes use the same Rust and Node toolchain setup as Codex: `~/.cargo/bin` on
PATH, the detected Homebrew Node prefix, private build output, and read-only
host toolchain stores. On macOS this also allows the public `/private/etc/ssl`
configuration and certificates and selects Command Line Tools when installed.
For read-only lanes, Rust setup is skipped when its executable directories
are absent; public TLS files, Command Line Tools and detected Node tools remain
available. It does not inherit arbitrary host PATH entries.

`--network off` retains the previous tool selection and toolchain grants.
Read-only lanes gain no workspace or shared-cache writes. Credential and
keychain restrictions, including the existing networked read-only `gh` policy,
still apply.

Shell-capable lanes expose the host's `~/.cargo/bin` on `PATH` and grant it
read and execute access, but never write access. This makes an installed
`cargo-nextest` available through `cargo nextest`; install it on the host with
`cargo install cargo-nextest --locked`. `provenant hosts doctor` reports
whether `~/.cargo/bin/cargo-nextest` is executable for lanes.

## Agy accounts

Agy has no account switch, so a second Google account gets its own `HOME`
holding its own login keychain. The registered `directory` is that home. A
directory other than your own home is an isolated account: the lane's private
home links its `Library/Keychains/login.keychain-db` instead of the user's login
keychain, the grant covers exactly that path, and registration and the quota
probe run `agy -p /usage` with `HOME=<directory>`. If that file is missing or is
not a regular file (a symlink does not count, nor does a symlinked `Library`
or `Library/Keychains` along the way, a hard-linked file, or a file that is the
same inode as your own login keychain, which is only stat'ed), registration,
launch and the quota probe fail with an error naming the path rather than fall
back to the login keychain. Both probes re-check the keychain (same device and
inode) immediately before spawning agy and again when it returns, and discard
the result if it changed. The probe is not sandboxed, so that guarantee covers
a keychain that is missing or changed when checked. The residual window is any
temporary replacement, absence or symlink swap while agy runs that is undone,
with the original inode restored, before the second check. Registering your own home directory keeps the shared
login keychain.

A registered isolated directory with no usable keychain is reported as
`KEYCHAIN_UNAVAILABLE` by `provenant quota` and by `provenant hosts doctor`, with
the expected path. If the directory was registered before isolated keychains
were required and represents the login-keychain account, re-register it with
`provenant accounts register --adapter agy --directory ~ <label>`. Otherwise,
create and sign in to the isolated keychain as described below. Launch errors
name the same path and repair options; missing or symlinked keychains continue
to fail closed.

Unregistered and registered agy behave differently at launch:

- With no agy registration, agy runs on the user's login keychain as before.
  Routes use the synthetic `local` account: nothing is probed or pinned, so a
  user who never registers an account sees no change.
- With a registration, agy accounts are pinned: a launch verifies the signed-in
  Google account against the registered key and skips the route if it differs
  (`changed identity; re-register it`). Register every account you route to.

Quota probes also verify the signed-in fingerprint. When it changes, the old
key is marked `account_mismatch` and excluded from routing. Provenant updates
that directory's registration to the new key under the config lock, preserving
its label and settings; the new account appears on the next quota pass. A
concurrent manual registration takes precedence; if the new identity is already
registered under another account, automatic rebinding is refused and Provenant
prompts to inspect accounts with `provenant accounts list` while keeping the stale
key excluded from routing. Dispatch can reuse a verified
fingerprint for 30 seconds, capturing keychain metadata before probing to guard
against concurrent sign-ins; a keychain metadata change or a new quota probe
invalidates the cached observation earlier. No credentials or email addresses
are stored in this cache.

Register the first account, the one signed in on your own login keychain,
with the home directory itself:

```text
provenant accounts register --adapter agy --directory ~ <label>
```

Add a second account (macOS; the empty keychain password and no lock timeout
are deliberate, so a headless lane never prompts). Every `security` command
below names the isolated keychain file explicitly, and none changes the real
default keychain:

1. `D=~/.agy-second; mkdir -p $D/Library/Keychains; chmod 700 $D`
2. `HOME=$D security create-keychain -p '' $D/Library/Keychains/login.keychain-db`
3. `HOME=$D security set-keychain-settings $D/Library/Keychains/login.keychain-db`
   (no lock timeout, no lock on sleep), then
   `HOME=$D security unlock-keychain -p '' $D/Library/Keychains/login.keychain-db`.
4. `HOME=$D agy`, run `/login`, finish the browser sign-in with the second
   Google account, then exit.
5. Check `HOME=$D agy -p /usage --output-format json` returns `SUCCESS`, and
   `HOME=$D security find-generic-password -s gemini -a antigravity $D/Library/Keychains/login.keychain-db`
   finds the item. The keychain path is the final argument, so the search does
   not fall through to another keychain on the search list; omit `-w` so no
   secret prints. Check the first account separately against the real login
   keychain with
   `security find-generic-password -s gemini -a antigravity ~/Library/Keychains/login.keychain-db`.
   The real login keychain is unchanged.
6. `provenant accounts register --adapter agy --directory $D <label>`, then
   `provenant quota`. Registration refuses a directory that has no regular
   `Library/Keychains/login.keychain-db` (a symlink does not count), so the
   second account can never be recorded under the first account's key.

The keychain file is plaintext-equivalent credential material: anyone who can
read it and unlock it holds the OAuth token, as with the shared login keychain.
Keep the account directory at mode 700 and out of backups and synced folders.

The isolated keychain locks again after a reboot. Repeat
`HOME=$D security unlock-keychain -p '' $D/Library/Keychains/login.keychain-db`
before routing to the account, or its launches fail the identity check. If the
sign-in is lost (revocation or Workspace re-authentication), repeat step 4.

## OpenCode

```text
opencode run --standalone --format json --log-level error [-s SESSION] -m MODEL[#effort]
```

It sends the prompt on stdin and uses the user's real HOME and XDG config/data
directories so configured providers and stored accounts remain available.
Inline configuration sets an empty `mcp` object and restricts editing, shell
and web access according to the access mode. The adapter sets
`OPENCODE_DISABLE_AUTOUPDATE`, `OPENCODE_DISABLE_PROJECT_CONFIG`,
`OPENCODE_DISABLE_CLAUDE_CODE` and `OPENCODE_DISABLE_FILEWATCHER`.

The Console's free models use the derived `zen` account, separate from the
`go` subscription account. Lanes and telemetry use this identity, and routing
paces Zen from its own quota observations rather than Go's. A
`[domains.routing.pacing.accounts.go]` profile applies only to Go lanes; Zen
uses the `zen` profile or the free-provider default. The existing free
pool remains unchanged. Quota lists both accounts when an
OpenCode account is configured; Zen reports only observed free-limit refusals.
Its allowance and daily reset remain unknown until a provider refusal supplies
a reset.

## Writer lanes

Fabric sets `CARGO_TARGET_DIR`, `CARGO_BUILD_BUILD_DIR` and `CARGO_HOME` to
lane-private paths. Do not override them: confinement may deny other paths.
Writers can set both build directories to `PROVENANT_LANE_CARGO_LINT_TARGET`
for lint-only runs, avoiding the test build lock. Read-only lanes use scratch
and build offline (`CARGO_NET_OFFLINE=true`) without Cargo's automatic cache
clean-up. Pinning the build directory overrides inherited Cargo configuration.
`provenant worktree remove` and `provenant gc` prune writer caches.

Each writer lane also holds a private block of ports for its dev servers and
tests, so parallel lanes do not collide. The lane sees it as
`PROVENANT_LANE_PORT_BASE` (the first port) and `PROVENANT_LANE_PORT_COUNT`
(how many consecutive ports it owns), for example `30032` and `16`. Blocks come
from one host-wide range, `lane_port_base` (default 30000), `lane_port_block`
(default 16) and `lane_port_blocks` (default 64), so the default range is
30000 to 31023; set them under `[domains.fabric.lanes]` (see
[configuration](configuration.md)) and set `lane_port_blocks = 0` to turn
allocation off. The guardian takes the first free block with an advisory lock
on `<state>/lane-ports/block-FIRST-SIZE.lock` just before the provider starts.
The provider and everything it starts inherit the lock descriptor, so the block
stays held until the whole process tree has exited, even if the guardian dies
first. Two live lanes never share a port, across every project on the same
state directory, even when `lane_port_base` or `lane_port_block` changes between
launches: a block is skipped while a held block of another layout overlaps it.
The kernel drops the lock when the last holder exits, so there is no leaked
range to reclaim and `provenant gc` has nothing to do; a later lane takes the
same block again. A writer launched while every block is held does not start and reports
`no free lane port block`; raise `lane_port_blocks`. Read-only lanes get no
block. The block is a convention, not a firewall: a lane can still bind outside
it, and an unrelated program can already be listening inside it.

Node lanes run the host's installed pnpm. Lanes redirect the npm and pnpm
caches and the Corepack home into the lane cache, but leave `PNPM_HOME` and the
pnpm store unset: pnpm records the store directory in
`node_modules/.modules.yaml`, so a lane override makes a worktree the chair
installed natively look stale. The default store (`~/Library/pnpm` on macOS,
`~/.local/share/pnpm` on Linux) is readable, never writable, and
`verify-deps-before-run` is off, so `pnpm run <script>` uses the native install
without reinstalling. Once a writer's head and route are verified, and before
its provider starts, Provenant runs `pnpm install --frozen-lockfile` on the
host when the worktree has `pnpm-lock.yaml` and no `node_modules/.modules.yaml`
(pnpm's completion marker), so a partial `node_modules` is repaired. The
install resolves pnpm using the host account's login-shell PATH, and passes
that PATH to pnpm so it can also find node when the peer daemon has a narrower
PATH. The login-shell probe runs from the account home, is bounded to ten
seconds, and is cancelled with the dispatch. If pnpm is missing,
`dependency_tool_not_found` names the searched PATH; install pnpm on that host
and check `pnpm --version` in its login shell before retrying. If the probe fails, preparation falls back to the host process PATH and records
`daemon_fallback` as the PATH source in the dependency preparation outcome; a
missing pnpm refusal also names that source and the searched PATH. If no daemon
PATH is available, `dependency_path_unavailable` reports the probe failure.
Cancellation stops preparation without falling back.
The install is bounded, runs outside the sandbox, and is cancelled with the
dispatch, with its process group cleaned up. Disable it with
`--knob lanes.prepare_deps=false` or the `lanes.prepare_deps` config key. An
install failure refuses the lane and includes the command, the exit status or
signal, and the end of pnpm's stderr. Corepack neither follows a project's
`packageManager` field nor downloads a version, so a pinned pnpm cannot arrive
as a non-executable `@pnpm/exe` placeholder. The native install uses the host's
shared pnpm store; a lane cannot write that store from inside its sandbox.

Read-only lanes with a verified Git checkout use a private scratch object
directory, with the host's common object store as a read-only alternate. The
shell Git wrapper selects it only for the bound repository and its linked
worktrees, for `show`, `log`, `diff`, `merge-tree` and `hash-object`; other
repositories retain their own stores. Aliases and external subcommands receive
no override; direct absolute-path Git calls bypass the wrapper. Git can create
temporary objects for commands such as `show --remerge-diff` without writing
the shared store. Scratch objects
and alternates are lane-controlled, so lane Git output is not trusted host
evidence. Scratch is removed at normal settlement; lost attempts retain it
until later cleanup.

On macOS, lanes cannot create hard links (`deny file-link`), and launch does
not walk the worktree for existing ones: hard-linked files in `target/`,
`node_modules/` or `.pnpm` neither refuse a launch nor become read-only. Launch
checks that the host Git `config`, `index`, `hooks/*` and `info/*` have a single name and refuses,
naming the file, if one has another; remove the other name and retry. A lane's
Cargo and pnpm caches are private to it, so a link inside them cannot reach a
shared store.

A writer's `TMPDIR` lives in the attempt's `tmp/candidate-N`, which terminal
commit and unreserved reconciliation collect. Account probes still use `scratch/`.
On macOS the lane sees that directory as `/private/tmp/pv-<hex>/candidate-N`,
a symlink to the attempt's `tmp` whose short path keeps Unix-domain socket names
under the 104-byte `sun_path` limit (see Lane lifecycle). Writer profiles permit
reading and executing fixtures only in that candidate's
temp subtree, signalling their own children and reading `/dev/fd/N`. Read-only
profiles keep their existing limits. Private check homes honour `TMPDIR`.

From `tools/front-door`, verify touched crates with
`cargo nextest run --profile lane -p <crate>`. The `lane` profile excludes
fixtures requiring hard links or short Unix socket paths that the writer
sandbox cannot provide.
Keep local checks targeted; full suites run in CI with the `ci` profile,
which retains these host-only tests.

A writer can exec build scripts and other binaries under its write roots
(`--write-root DIR` or `writer_writes`) and its own attempt temp subtree.
Other writable state stays non-executable. An env var alone grants nothing,
and the lane then fails with "operation not permitted". Fabric, manifest, nested
and `--host` dispatch cannot pass `--write-root`, so declare the directory in
`[domains.run] writer_writes` instead, in the peer's own config for `--host`;
see [configuration](configuration.md#writer-extra-write-paths). Its entries are
used as written and must be real paths (`/private/tmp`, not `/tmp`).

Inspect `provenant status RUN/TASK/ATTEMPT --json` and its `write_grants`
array for the write allowances applied to that attempt. Each entry records its
source, scope and absolute path. The path is stored relative to `/`, as
`{"relative_to_root":{"encoding":"utf8","path":"Users/..."}}`, or with
`"encoding":"native_bytes"` and a hex `path` when it is not UTF-8. A
`write_root` source covers both `[domains.run] writer_writes` and
`--write-root`. Writer Git grants are represented by `git_grants_digest`;
profile deny rules still restrict the listed allowances.

Run refuses an attempt whose grant list exceeds 1,024 entries or 256 KiB
encoded. The attempt ends Rejected before launch and its siblings still start.

Remote-tracking refs, their reflogs and shared config are read-only in writer
lanes. The host refreshes origin before launch; a fetch that tries to update a
tracking ref is denied. Dispatch with `--deliver push` to have run push the verified final commit to
the lane branch after the lane finishes.

Network-open profiles can read the existing `~/.config/gh` login and may use
it to push; profiles deny access to `~/.ssh`. After a successful push,
Git may report that it could not update its local tracking ref after a successful
push, or set an upstream with `-u`, because shared Git state is read-only. The host
best-effort refreshes the lane branch's `origin` tracking ref at settlement;
shared upstream configuration remains read-only.

## Native lane probes

The macOS native writer capability invocation runs named socket, credential,
Git (including stash and rebase), ripgrep, Cargo, nextest, Swift, Python, shell and
Node/pnpm probes independently and reports their failures together. It also
removes the compiled scratch execution grant and checks that the temporary
script fails while Python and the here-document still pass. It currently
exercises the Claude writer profile; it is not yet a complete adapter and
access-mode matrix.

When a lane's decoded answer or captured stdout or stderr contains a path-bearing
permission error, the settlement reason includes a bounded excerpt identifying the
path whose access grant may be missing. The answer itself stays as the provider
wrote it. The error alone does not establish which rule denied
the operation: filesystem permissions or another sandbox may be responsible.
Errors kept only in an unreported provider transcript are not visible there.

## General guidance

Use the Rust provider source for launch behaviour. Dispatch still requires an
installed executable, local login and the applicable routing and confinement grants.

## Cloud environment preparation

`scripts/cloud-environment [REPOSITORY]` prints a starting checklist for a
provider environment, including detected tracked Cargo manifests. Run it from
any repository or pass its path; adapt the toolchain, native dependencies,
nextest installation and optional build warm-up to the selected tests. Provider
setup time limits may make a warm build impractical. Create and select the
environment in the provider's interface. Do not put credentials in setup
scripts, prompts or result artifacts.

## Cloud placement

Configure a named target in Provenant's config. The provider environment and
the local Git `origin` must refer to the intended repository. Install and log
in to the selected provider CLI separately; do not put credentials in target
settings, prompts, setup scripts, log URLs or result artifacts.

```toml
[domains.lanes.cloud_targets.verify]
provider = "codex" # or "claude"
environment = "environment-id"
repository = "owner/repository"
branch = "main"
model = "requested-model"
effort = "medium"
deadline_seconds = 3600

[domains.lanes.projects."PROJECT_ID"]
cloud_target = "verify"
```

The project setting is optional. Without it, choose the target explicitly:

```sh
provenant run --file cloud-verify.toml --explain
provenant reconcile --id ATTEMPT_ID
provenant status ATTEMPT_ID
provenant output ATTEMPT_ID
provenant output ATTEMPT_ID
provenant stop ATTEMPT_ID
```

`--placement local` overrides a project's cloud default. The target's branch
defaults to `main` and its deadline to 3600 seconds. Use `--cloud-provider`,
`--cloud-environment`, `--cloud-repository`, `--cloud-branch`, `--cloud-model`,
`--cloud-effort` and `--cloud-deadline` for per-run overrides. `--head` selects
the full commit; otherwise the current HEAD is used. `--wait` observes the job
before returning. Cloud jobs are read-only verification work and cannot take a
writer or delivery target.

The owner saves the intent before submission and the remote ID before waiting.
After a restart, `reconcile` observes saved jobs; `status`, `status` and `output`
show the saved observation and its result. Cloud lane states are prefixed
`cloud_`. In JSON status, `cloud_state` and `cloud_jobs` describe the remote
job; the underlying local attempt has no process exit. A changed tracked file
makes the result fail. The views report the requested model separately from an
unknown or different observed model. Unknown cost produces a warning.

Claude has no status CLI: an unfinished job stays running until its deadline,
then becomes unknown. It publishes a result-only commit on a `claude/*`
branch, which the owner removes on a best-effort basis after saving the result.
Codex returns the result in its cloud diff and cannot request model or effort.
Cancellation is unconfirmed for both providers. CLI output formats have not
yet been captured on real hosts; a parsing failure reports what was seen.

The job can use `scripts/cloud-verify` to run its named verification command.
It requires the job ID, nonce and full commit from the request, a test name,
a local log file and a credential-free log location. It writes
`.provenant-cloud-result.json` with the exit status and tracked-file status;
`--observed-model` may record a model visible to the VM. The owner checks the
job ID, nonce and commit. The nonce is advisory, not a credential or proof of
origin. Cloud results do not replace exact-head required CI.
