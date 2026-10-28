# Providers
Owner: provenant-providers
Anchor: tools/front-door/crates/providers/src

Per-adapter launch notes. All five adapters (Claude, Codex, Cursor, Agy and
OpenCode) are enabled by default in every access and session mode with a launch
form; recordings serve replay tests only. Use `provenant route list`
to inspect the catalogue; readiness is decided at dispatch.

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

A confined writer's environment is built from scratch and sets `CARGO_TARGET_DIR`
and `CARGO_BUILD_BUILD_DIR` to its per-lane cache's `target` directory. Read-only
lanes set both to `target` under their scratch directory, and build offline
(`CARGO_NET_OFFLINE=true`) without Cargo's automatic cache clean-up. Pinning build-dir
overrides inherited Cargo configuration and keeps intermediate artefacts in the
lane. Share a target only within one worktree; across
worktrees keep per-lane targets (`provenant worktree remove` and
`provenant gc` prune the lane caches), because a target shared across
worktrees can run another checkout's stale test binaries.

On macOS, launch inspects the whole writable worktree, including ignored
`target/` and `node_modules/` directories. Existing hard-linked regular files
stay readable, but each in-tree alias is write-denied and its ancestor
directories cannot be renamed. Other files remain writable. The census allows
500,000 entries and at most 4,096 denied paths; the profile with those denials
is bounded to 512 KiB. If the denials exceed either limit, launch refuses with
recovery advice: move disposable build/dependency output out of the worktree
(for Rust, set `CARGO_TARGET_DIR` outside it), then retry. Important files and
links whose ownership is unclear should be preserved. Other censused writable
roots, such as private scratch and Git objects, still refuse hard-linked files.

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

Remote-tracking refs and shared config are read-only in writer lanes, so
`git fetch` of a configured remote fails. Fetch on the host before dispatch,
or dispatch with `--deliver-remote` and `--deliver-branch`: run fetches before
launch and pushes the verified final commit after.

Network-open profiles can read the existing `~/.config/gh` login and may use
it to push; profiles deny access to `~/.ssh`. After a successful push,
Git may report that it could not update the local remote-tracking ref or set an
upstream with `-u`. These denials are expected and need no report. The host
best-effort refreshes the lane branch's `origin` tracking ref at settlement;
shared upstream configuration remains read-only.

## General guidance

Use the Rust provider source for launch behaviour. Dispatch still requires an
installed executable, local login and the applicable routing and confinement grants.
