# Providers

Per-adapter launch notes. All five adapters (Claude, Codex, Cursor, Agy and
OpenCode) are enabled by default in every access and session mode with a launch
form; recordings serve replay tests only. Use `provenant route list`
to inspect the catalogue; readiness is decided at dispatch.

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

A writer's `TMPDIR` lives in the attempt's `tmp/candidate-N`, which terminal
commit and unreserved reconciliation collect. Account probes still use `scratch/`.

From `tools/front-door`, verify touched crates with
`cargo nextest run --profile lane -p <crate>`. The `lane` profile excludes
fixtures requiring hard links, scratch executable access, child signals,
`/dev/fd` or short Unix socket paths that the writer sandbox cannot provide.
Keep local checks targeted; full suites run in CI with the `ci` profile,
which retains these host-only tests.

A writer can exec build scripts and other binaries under its write roots
(`--write-root DIR` or `writer_writes`), but not in scratch, its `TMPDIR` or
other state it can write. An env var alone grants nothing, and the lane then
fails with "operation not permitted". Fabric, manifest, nested and `--host`
dispatch cannot pass `--write-root`, so declare the directory in
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

Writers hold no Git credentials, and remote-tracking refs and shared config are
read-only, so `git fetch` of a configured remote fails. Fetch on the host
before dispatch, or dispatch with `--deliver-remote` and `--deliver-branch`:
run fetches before launch and pushes the verified final commit after.

## General guidance

Use the Rust provider source for launch behaviour. Dispatch still requires an
installed executable, local login and the applicable routing and confinement grants.
