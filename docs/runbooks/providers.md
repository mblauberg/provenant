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
to its per-lane cache. Share a target only within one worktree; across
worktrees keep per-lane targets (`provenant worktree remove` and
`provenant gc` prune the lane caches), because a target shared across
worktrees can run another checkout's stale test binaries.

A writer's `TMPDIR` lives in the attempt's `tmp/candidate-N`, which terminal
commit and unreserved reconciliation collect. Account probes still use `scratch/`.

A writer can exec build scripts and other binaries under its write roots
(`--write-root DIR` or `writer_writes`), but not in scratch, its `TMPDIR` or
other state it can write. An env var alone grants nothing, and the lane then
fails with "operation not permitted". Fabric, manifest, nested and `--host`
dispatch cannot pass `--write-root`, so declare the directory in
`[domains.run] writer_writes` instead, in the peer's own config for `--host`;
see [configuration](configuration.md#writer-extra-write-paths). Its entries are
used as written and must be real paths (`/private/tmp`, not `/tmp`).

Writers hold no Git credentials, and remote-tracking refs and shared config are
read-only, so `git fetch` of a configured remote fails. Fetch on the host
before dispatch, or dispatch with `--deliver-remote` and `--deliver-branch`:
run fetches before launch and pushes the verified final commit after.

## General guidance

Use the Rust provider source for launch behaviour. Dispatch still requires an
installed executable, local login and the applicable routing and confinement grants.
