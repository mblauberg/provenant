# Providers
Owner: provenant-providers
Anchor: tools/front-door/crates/providers/src

Per-adapter launch notes. All five adapters (Claude, Codex, Cursor, Agy and
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

Only effort can refuse today. Resume, network and write never refuse for any
adapter; they are declared so a future adapter can narrow them. Steering (input
into a live session) and image input are declared likewise, and no request
surface exists for either yet. Cursor has no
effort control on its command line, so its catalogue models record none.

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
