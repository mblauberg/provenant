# Providers
Owner: provenant-providers
Anchor: tools/front-door/crates/providers/src

Per-adapter launch notes. The model adapters (Claude, Codex, Cursor, Agy and
OpenCode) are enabled by default in every access and session mode with a launch
form; recordings serve replay tests only. Use `provenant route list`
to inspect the catalogue; readiness is decided at dispatch.

## Launch capabilities

One declared table per adapter (`AdapterSpec::launch` in
`tools/front-door/crates/records/src/adapters.rs`) is the source for dispatch,
`run`, resume and graph validation. A request outside it is refused with the
adapter and field named (`cursor does not support effort`) before admission,
worktree preparation, reservation or receipt creation. A named route is checked
once routing resolves its adapter; a model's recorded effort controls in the
catalogue still apply on top.

| Adapter  | Effort | Resume | Steering | Network modes          | Image input | Write |
| -------- | ------ | ------ | -------- | ---------------------- | ----------- | ----- |
| claude   | yes    | yes    | no       | provider, local, open  | no          | yes   |
| codex    | yes    | yes    | no       | provider, local, open  | no          | yes   |
| cursor   | no     | yes    | no       | provider, local, open  | no          | yes   |
| agy      | yes    | yes    | no       | provider, local, open  | no          | yes   |
| opencode | yes    | yes    | no       | provider, local, open  | no          | yes   |
| command  | no     | no     | no       | off, local, open       | no          | no    |
| host-native | configured | no | no | provider, local, open | no | yes |

Command routes have no model and refuse model, effort, resume and write
requests. Steering (input into a live session) and image input have no request
surface yet. Cursor has no effort control on its command line, so its
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
disabled, and ordinary `resume` can dispatch a fresh lane with the prior result.
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
`provenant dispatch --route command/fmt-check --prompt-file P`. The applied
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
pool and free-model opt-in remain unchanged. Quota lists both accounts when an
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
without reinstalling. Corepack neither follows a project's `packageManager`
field nor downloads a version, so a pinned pnpm cannot arrive as a
non-executable `@pnpm/exe` placeholder. Install dependencies on the host before
dispatch; a lane cannot install into the shared store.

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
Writer profiles permit reading and executing fixtures only in that candidate's
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

Inspect `provenant lanes --id RUN/TASK/ATTEMPT --json` and its `write_grants`
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
tracking ref is denied. Dispatch
with `--deliver-remote` and `--deliver-branch` to have run push the verified
final commit after the lane finishes.

Network-open profiles can read the existing `~/.config/gh` login and may use
it to push; profiles deny access to `~/.ssh`. After a successful push,
Git may report that it could not update its local tracking ref after a successful
push, or set an upstream with `-u`, because shared Git state is read-only. The host
best-effort refreshes the lane branch's `origin` tracking ref at settlement;
shared upstream configuration remains read-only.

## General guidance

Use the Rust provider source for launch behaviour. Dispatch still requires an
installed executable, local login and the applicable routing and confinement grants.
