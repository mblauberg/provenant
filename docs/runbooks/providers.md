# Providers

Per-adapter launch notes. All five adapters (Claude, Codex, Cursor, Agy and
OpenCode) are enabled by default in every access and session mode with a launch
form; recordings serve replay tests only. Use `provenant route list`
to inspect the catalogue and current availability.

## OpenCode

```text
opencode run --standalone --format json --log-level error [-s SESSION] -m MODEL[#effort]
```

It sends the prompt on stdin and uses the user's real HOME and XDG config/data
directories so configured providers and stored accounts remain available.
Inline configuration sets an empty `mcp` object and restricts
editing, shell and web access according to the access mode. The adapter sets
`OPENCODE_DISABLE_AUTOUPDATE`, `OPENCODE_DISABLE_PROJECT_CONFIG`,
`OPENCODE_DISABLE_CLAUDE_CODE` and `OPENCODE_DISABLE_FILEWATCHER`.

## Writer lanes

A confined writer's environment is built from scratch and sets `CARGO_TARGET_DIR`
to its lane cache. A shared target is usable only when it is declared with
`--write-root DIR` and the lane's commands set
`CARGO_TARGET_DIR=DIR`: the write root grants write and exec, so build scripts
under it run. An env var alone grants nothing, and the lane then fails with
"operation not permitted". Share a target only between lanes that may share its
output.

A writer cannot `git fetch` over HTTPS: its sandbox holds no credentials and
Git refs and remote config stay immutable. By design the host fetches, then
delivers the result.

## General guidance

Use the Rust provider source for launch behaviour. Dispatch still requires an
installed executable, local login and the applicable routing and confinement grants.
